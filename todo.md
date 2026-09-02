# Viso — TODO

> Working memory so progress survives interruption. Update as sections land.
> Authority order: accepted ADR > architecture doc > AGENTS.md > crate docs > convention.
> Standing constraints (do not drop):
>
> - No Makepad-keyword comments in code. No `§`+number section symbols in code/comments; strip any you pass.
> - Reference `/Users/x/code/vizo/makepad` code when designing a subsystem; don't invent alone.
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
derivation system: it uses redraw*id generation + explicit invalidation queue +
draw-cache memo; Effect-equivalent is imperative handle\*\* hooks + apply-reload
re-eval + async drop→GC cancellation; Animator = NextFrame self-requeue loop
that snapshots start values, restarts on state switch, stops when done). Viso
deliberately diverges to compiled/tracked fine-grained reactivity; we take the
*semantics\* (re-eval only on dep change, propagate only if result changed;
dep-change restart = cleanup-then-rerun; unmount cleanup) not the coarse model.
New file `crates/ui/src/reactive.rs` (Computed + Effect are one subsystem).
Since makepad has no reactive-derivation system to preserve, we chose the better
fine-grained design outright rather than fitting a coarse model: compiled binding
fast path + runtime `DepCursor` fallback, and — decisively — Computed and Effect
wake by DIFFERENT routes because their outputs differ in kind (see below).

- [x] `Computed` (pure; runtime dep collection via a `DepCursor` passed into eval — not thread-local; a read-only `ComputeCx` exposes `get` only, so purity is enforced by the type, no `set`). SoA `ComputedStore` keyed by generational `ComputedId`: cached last `StateValue` + dep set. `eval` records deps → refreshes them into `ComputedStore`'s own `StateId -> Vec<ComputedId>` reverse index (NOT `bind_dynamic` — the wrap-up dropped that route because the binding flush dirties dynamic edges unconditionally, bypassing the memo boundary). Returns `changed`. `wake_computed` re-evals affected derivations and marks the downstream node dirty ONLY when the value changed (the memo boundary in the wake). A Computed's output _is_ a node dirty class.
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
> the authority. The old "Phase 3" section above was an _execution_ label that bundled
> the doc's Phase 3 (NodeArena — done as Slice A's `NodeStore`/arena), the doc's Phase 4
> _layout_ (Slice A: Component→Node→Flex→Paint, i.e. doc-§69 items 5–6 box-constraints +
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

**Before each subsystem:** read the Makepad reference at `/Users/x/code/vizo/makepad` for that
subsystem (hit-test/finger/pointer, focus/key/next-frame, area/DrawList bounds) as a
_semantics_ reference only, then design on Viso Node/State/Dirty contracts. Makepad's model
is coarse (event walk over the widget tree, area-compare); Viso takes the target-route
semantics (capture→target→bubble, focus ring, IME preedit) not the coarse walk.

### Slice C — Input: bounds/transform + hit test ✅

Doc items: world bounds/transform → hit test. Layout already bakes _absolute_ positions into
`NodeStore::bounds` (top-down placement), so `bounds` is already the world rect for the current
model — no local→world transform to accumulate, and no read-back indirection. Slice C ships the
direct forward path; a distinct world/transform column earns its place only once scroll/clip/
transform containers exist (deferred to the Scroll slice, see below).

- [x] `Rect::contains(px, py)` point-in-rect primitive: near edges inclusive, far edges
      exclusive (`[x, x+w)` / `[y, y+h)`) so tiling siblings do not both claim a shared seam.
- [x] Per-node `hittable` flag: hot SoA column on `NodeStore` aligned to `bounds`/`dirty`,
      default `true`, maintained in lockstep in `clear()`/`alloc()`. `hittable`/`set_hittable`
      accessors (the latter guarded on liveness). A pass-through node opts out without a
      structural change.
- [x] `HitTestTree` (stateless query over `&NodeStore`) + free fn `hit_test`: descends children
      in **reverse** sibling order (the mirror of `paint_tree`'s forward walk, so the visually
      topmost child wins) and prunes any subtree whose box does not contain the point — a
      targeted route, not a full-tree scan. Zero allocation, depth-bounded recursion.
- [x] `HIT_TEST` dirty class already exists (one of the 8); no new class. Because `bounds` _is_
      the hittable rect this slice, the incremental relayout that writes `bounds` already
      re-derives what a node hits — no extra recompute pass.
- [x] Headless tests: point over a leaf returns that leaf; nested overlap returns the topmost;
      point in padding returns the container (or nothing after `set_hittable(_, false)`); point
      outside the root box returns `None` (prune proof); seam between tiling siblings hits
      exactly one. Plus a facade integration test through `viso::ui`.
- [x] `§`/Makepad refs stripped from every touched file (incl. pre-existing ones in
      `primitive.rs`). Committed.
- Deferred to the **Scroll slice** (not built now — would be a duplicate column that is pure
  overhead while `bounds` is already absolute): a distinct world/transform column recomputed on
  HIT_TEST/TRANSFORM/LAYOUT dirt; clip folding carried down the descent; the "translated subtree
  hit-tests at its world position" and "clip rect excludes outside points" cases. The descent is
  written so the swap is localized (read a `world(id)` accessor instead of `bounds(id)`, intersect
  against a clip carried down the recursion).

### Slice D — Input: pointer routing (capture → target → bubble) ✅

The normalized-event → route pipeline: hit-test picks the target, dispatch walks the
ancestry, a handler's `set` lands pending, and the existing flush turns it into targeted
node invalidation.

- [x] Normalize platform pointer events into a runtime-tier `PointerSample` at the scheduler
      seam: the scheduler owns the window, so it is the one place that resolves scale and
      converts the logical-point raw sample into physical-pixel space once, before the driver
      sees it. Phases map down/move/up/leave; modifiers/buttons carried through. Runtime uses
      its own value mirrors, not platform's — no OS vocabulary leaks up.
- [x] Route: hit-test the target once, build the root→target ancestry chain, then dispatch
      capture (root→below-target), target (once), bubble (below-target→root). Each ancestor
      handler runs twice, the target's once; a lone target (chain of 1) runs only target. The
      route carries a phase-specific `EventCx` that reads state and defers writes (get/set),
      nothing layout or GPU.
- [x] Handler registration: a node declares a pointer handler through `BuildCx::on_pointer`;
      stored in a compact cold per-node handler column keyed by node index — no per-event
      `dyn` walk of the whole tree, no string child lookup. The router moves the handler box
      out to call it (`take_handler`/`restore_handler`), so no interior mutability. A click
      that `set`s state flows into the existing flush.
- [x] Facade: `on_input` is no longer empty — it lowers the runtime sample into the ui-tier
      `PointerEvent`, routes it through the retained tree with a reusable ancestry buffer
      owned on the driver (zero steady-state alloc), and lets the resulting `set` ride the
      next frame's flush. The scheduler flags the input frame dirty, so that flush runs.
- [x] Headless tests: unit tests over the router prove capture/target/bubble order and the
      lone-target case; a facade integration test (`crates/viso/tests/pointer_routing.rs`)
      proves the full chain end to end — a synthetic click flips a bound state cell, the write
      lands pending, and the flush dirties exactly the bound node (PAINT) and leaves the
      unbound container clean. A miss dispatches nothing.
- [x] Stripped section/Makepad refs from every file touched; committed per section.

**Deferred from Slice D (recorded, not built — no over-engineering ahead of need):**

- **Pointer capture (drag).** No cross-frame capture holder yet: a node cannot yet claim
  subsequent moves/up regardless of hit. Lands when the first draggable control (or the
  Scroll slice's drag-to-scroll) actually needs a held target across frames.
- **`stop_propagation`.** The route currently always runs the full capture+target+bubble
  walk. A handler cannot yet halt propagation. Lands with the first widget that must swallow
  an event (e.g. a modal backdrop, a button consuming its click).
- **Scroll / key / text samples.** Only the `Pointer` `InputSample` variant exists; the
  scheduler still flags scroll/key/text frames dirty but threads no sample. Scroll lands with
  the Scroll slice; key/text with Slice E.
- **hover / enter / leave.** No pointer-tracking state to synthesize enter/leave transitions
  from move samples yet. Lands with hover styling / cursor feedback.

### Slice E — Input: keyboard / focus / IME

Doc §69 item 4.

- [x] Focus: a single focused `NodeId` slot on `NodeStore` + a `focusable` cold flag column
      (opt-in, default false); `focus_next(store, root, forward)` pre-order ring traversal that
      wraps; programmatic focus request via `EventCx::request_focus`/`clear_focus`, applied by
      the router after the handler returns. Focus change is targeted PAINT on old+new only
      (PAINT is non-bubbling, so the container is never over-invalidated).
- [x] Keyboard: normalized key events routed to the focused node with capture/target/bubble
      via a shared `dispatch_chain` — the exact ancestry walk pointer routing uses, factored
      out; only the target source (focus vs hit-test) and the handler column (`key_handlers`
      vs `handlers`) differ. `KeyRouter::route_key`.
- [x] IME: preedit/commit events routed to the focused node via `KeyRouter::route_ime`; the
      handler reads the composition string + caret through `EventCx::ime()` (and key transitions
      through `EventCx::key()`) — enough of a text-input-shaped surface for `07-text-input` later.
      IME plumbed platform (`RawEvent::ImePreedit`) → runtime (`InputSample::ImePreedit`/`Text`)
      → facade → ui in normalized form.
- [x] Headless tests: `focus_next` steps in order and wraps + skips non-focusable; a key event
      reaches the focused node's handler and bubbles (`[0,1,0]`); no focus ⇒ no dispatch; focus
      change dirties only the two nodes' focus-ring paint; an IME preedit/commit sequence routes
      to the focused node; pointer/key handler columns are isolated; a focus request from a
      handler moves focus. (viso-ui unit tests + `crates/viso/tests/keyboard_routing.rs`.)
- [x] Strip `§`/Makepad refs from every file touched. Commit.

**Deferred from Slice E**

- **`stop_propagation`** — still deferred (shared with Slice D). Key/IME routing runs the full
  capture+target+bubble walk; a handler cannot yet halt it. Lands with the first widget that
  must swallow an event.
- **Focus on pointer-down** — clicking a focusable node does not yet focus it; focus moves only
  via Tab (`focus_next`) or programmatic `request_focus`. Lands with the first click-to-focus
  control.
- **Semantics of focus** — accessibility focus / SEMANTICS dirtying on focus change is Slice G;
  this slice's focus change is PAINT-only (the focus ring).

### Slice F — Style token ✅

Doc §69 item 9 (§14). Compile source style names to IDs; no runtime string lookup on the
hot path.

- [x] `TokenId`/`StyleId` interning: six semantic namespaces (`color.*`, `spacing.*`,
      `radius.*`, `typography.*`, `elevation.*`, `motion.*`) fold to compact ids via
      `TokenInterner` (cold string map, build-time only). A `Theme` maps each `TokenId` to a
      `StateStore` cell — so a token's _value_ lives in a normal state cell and resolution is
      a `Vec` index plus a state read, never a string map (§14/§29). (`crates/ui/src/token.rs`)
- [x] A node's `StyleId` references resolved tokens (`fill`/`radius`), not literals, folding
      onto its warm `BoxStyle` in place — so a theme swap re-resolves without touching node
      structure. A theme swap is a `StateStore::set` on the token's backing cell, riding the
      ordinary pending/flush path: it dirties only STYLE/PAINT of the nodes bound to that cell
      through the existing `BindingTable`. `NodeStore::resolve_styles` is the incremental STYLE
      pass; no new invalidation machinery. (`crates/ui/src/style.rs`, `component.rs`)
- [x] A normal frame with no token change recomputes no style cascade — `resolve_styles`
      returns 0 when nothing is STYLE-dirty (§14 exit), proven by `clean_frame_runs_no_style_cascade`.
- [x] Headless facade tests (`crates/viso/tests/style_token.rs`): token resolves to value;
      theme swap re-resolves bound nodes only; unrelated untokenized node untouched; token
      change dirties STYLE/PAINT not MEASURE/LAYOUT; clean frame runs no cascade.
- [x] Struck `§`/Makepad refs from every touched file. Committed per section.

**Deferred from Slice F**

- **Non-color/radius tokens** — only `fill` (a `color.*` token) and `radius` are tokenizable
  this slice, since those are all `BoxStyle` carries. `border`, and the `spacing`/`typography`/
  `elevation`/`motion` namespaces, stay literal until a consumer (a bordered widget, a text
  control, a shadow layer, an animator) lands; the namespaces already exist in the interner.
- **Cached dirty-node list for resolve** — `resolve_styles` scans all nodes for the STYLE mark
  each swap. A swap touches few nodes, so a cached bound-node list (like a future focus index)
  is the optimization if a huge tree ever makes the scan hot; deferred until measured.
- **Compile-time `theme.vs`** — tokens are interned at runtime here; the `.vs` theme source →
  AOT token table (§21.5/§32 `theme.vs`) is a DSL-slice concern, not this slice.
- **Style version / measure-affecting tokens** — a tokenized field that feeds MEASURE (e.g. a
  future `spacing` on a container) must dirty MEASURE+LAYOUT, not just STYLE+PAINT; the binding
  class is per-field and lands with the first such token's consumer.

### Slice G — Semantics

Doc §69 item 10 (§15). Accessibility tree generated from the Node model, incrementally.

- [x] `SemanticsTree` derived from nodes: role, label, state (focused), bounds.
      Generated from the Node model (§69 exit: "semantics 从 Node 模型天然生成"), not a
      parallel hand-maintained tree. `semantics.rs` value types (`Role`/`Semantics`/
      `SemanticsNode`/`SemanticsTree`); a cold authored column + `set_semantics` on
      `NodeStore`; `derive_semantics` folds the authored column with live state (focus
      slot, handler-derived roles, bounds) in a pre-order walk mirroring `paint_tree`.
- [x] Incremental: the existing SEMANTICS dirty class drives re-derivation; a
      focus/label/role change dirties SEMANTICS on that node and it bubbles to the root
      (`BUBBLING`), so `derive_semantics_dirty` re-derives on a semantic change and does
      nothing on a clean frame. (Whole-tree rebuild this slice — see deferral below.)
- [x] Interactive nodes (pointer/key handlers from Slices D/E) get a default `Button`
      role so a keyboard/AT path exists (§15); authored role wins over the default.
- [x] Headless semantics-snapshot tests (§35/§66) in `crates/viso/tests/semantics_snapshot.rs`:
      tree shape + roles for the demo scene; a focus change updates only that node's
      semantics; a label change re-derives one node; a clean frame derives nothing. Plus
      `BuildCx::semantics` authoring, the `viso::ui` exports, and the focus→SEMANTICS
      wiring in `input.rs` (focus move now marks `PAINT | SEMANTICS`).
- [x] Stripped `§`/Makepad refs from every file touched. Committed per section.

**Deferred from Slice G**

- **Per-subtree incremental caching** — the whole tree re-derives on any SEMANTICS dirt
  (SEMANTICS bubbles to root, so any change reaches `root`). A cached previous tree +
  per-subtree rebuild lands when a large tree makes the full walk hot — a §37 perf-measured
  refinement, not now. The exit criterion is met at the _invalidation_ level: a clean frame
  derives nothing; only a semantic change triggers a rebuild.
- **Text-node label invalidation** — a label on a _text_ node is MEASURE+LAYOUT+PAINT+SEMANTICS
  (§11); this slice's label is SEMANTICS-only because there is no text node yet. The MEASURE/
  LAYOUT classes join `set_semantics` (or the text widget's own setter) when the first text
  control lands.
- **Richer roles / state** — `Role` is `Group`/`Button`/`Label`; checked/expanded/value/range
  and the rest grow with the widgets that need them.
- **Platform AT bridge** — feeding the OS accessibility API from the `SemanticsTree` is a
  platform-tier concern (kept out of `viso-ui` per the §10 DAG); lands with a platform a11y
  slice, no `accesskit` dependency in the ui tier.

### Wrap-up for Phase 4

- [x] `01-counter` example: a real interactive counter (button click → `set` → bound label
      re-derives / bar repaints), the first end-to-end interactive Viso app. Headless input-tape
      test (`crates/viso/tests/counter.rs`) asserts the click drives exactly the counter's dirty
      classes (button SEMANTICS, bar PAINT), a miss is inert, and the semantics react.
      As built: shipped via the `Application::build` scene seam + `BuildCx::with_reactive`
      reactive reach + `EventCx::pointer()` phase access — see ADR 0006.
- [x] ADR 0006 — Application scene seam (`Application::build`, default empty, replacing the
      hardcoded demo `Scene`), `BuildCx::with_reactive` build-time reach into the reactive
      stores, and the `EventCx::pointer()` pointer-phase access decision (gate a click on
      `PointerPhase::Up`). Records the §13 target-route divergence from Makepad's event-walk and
      the §6.4 phase-specific-context contract. (Scope narrowed from the earlier broad
      "input & focus + style + semantics" framing to exactly what this wrap-up shipped; focus/IME
      routing, style-token resolution, and semantics derivation are already covered by their own
      slices and ADR 0005 / Slice G.)
- [x] Reconcile architecture doc §69 with "As built" notes pointing at ADR 0006 (above): the
      Phase 4 interactive-scene exit is met — an app authors its retained tree in `build`, a
      pointer click drives targeted reactive invalidation with no full-tree rebuild, verified
      headlessly. Deferred Scroll/VirtualList/Grid remain the only §69 items outstanding (below).
- [x] Slice H — Scroll (§69 item 7). DONE.
      Spec: `docs/superpowers/specs/2026-09-02-phase4-slice-h-scroll-design.md`.
      As built: `LayoutInput::Scroll { axis, size }` viewport laying out a single content child at
      its natural extent along `axis` (overflow scrolls); `NodeStore` scroll columns `scroll`
      (offset, nonzero only on viewports), `world` (scrolled world rect, derived), `content`
      (viewport content extent), and `capture` (pointer-capture holder). `scroll_by(id, delta)`
      clamps per axis to `[0, scroll_range]` and marks `TRANSFORM | HIT_TEST | PAINT` — never
      LAYOUT/MEASURE, never bubbles. `resolve_transforms(root)` (run at the end of `layout()`)
      pre-order derives `world = bounds − accumulated ancestor scroll`. Paint wraps viewport
      children in a `Layer{clip: world}` / `LayerEnd`; hit testing reads `world` and narrows the
      clip on entering a viewport. `ScrollEvent` + `ScrollRouter::route` select the innermost
      viewport under the pointer and chain nested scrollers per axis (allocation-free hit test +
      parent-link walk). Facade `on_input` lowers `InputSample::Scroll`. Pointer capture: the
      pointer router routes to the captured node when set, releasing only on explicit
      `release_pointer()`; `EventCx` gained capture/`stop_propagation` requests threaded through
      a shared `Dispatched { ran, stop }` return across the pointer/key/IME dispatch chain.
      Verified: `cargo xtask check-deps` (13 crates), fmt, clippy `-D warnings`, full `cargo test`
      (101 ui unit tests incl. scroll_by clamping, resolve_transforms world shift, scroll-clip
      paint layer, hit-test under scroll clip, axis-delta absorption, clamp, cross-axis, miss,
      nested-scroll split; 3 facade `scroll_routing` integration tests). No shader change, so no
      Metal validation needed.
- [x] Slice I — VirtualList (§69 item 8, §12.4). DONE.
      ADR: `docs/adr/0008-virtual-list-viewport-reconcile-and-recycle.md`.
      As built: a VirtualList *is* a `LayoutInput::Scroll { axis, size }` viewport (so ADR 0007's
      `scroll_by`/`scroll_range`/`ScrollRouter`/clip/hit-test are reused unchanged) over a single
      content child = an `AbsoluteRows { axis, size }` canvas sized `Fixed(total_extent)` main /
      `Fixed(cross)` — the canvas's own fixed extent is the spacer, so `scroll_range == total −
      viewport` is correct without mounting all items. New `NodeStore` warm column `row_offset:
      Vec<f32>` (sentinel = not a positioned row), read via a `LayoutTree::row_offset` hook; the
      `AbsoluteRows` layout arm places each mounted child at `main = row_offset(child)`, cross =
      fill. Data model in `crates/ui/src/virtual_list.rs`: `HeightTree` (Fenwick/BIT over f32,
      O(log n) `prefix_sum`/`point_query`/`update`/`total`/`find_position`/`resize`/
      `update_default_height`), `HeightCache` (running mean, fallback = estimate),
      `VirtualListState` (height tree+cache, index anchor, window, `mounted`, `pool`, boxed
      builder, canvas id, reused scratch), and `VirtualLists` (`Vec<Option<Box<..>>>` keyed by
      viewport `NodeId::index()` — dense index, no hash; driver-owned, not a NodeStore column, not
      a per-node HashMap). `virtual_list::reconcile(store, lists, states, bindings, effects)` runs
      in `FramePhase::Layout` **before** `relayout_and_paint`, gated to a no-op when
      `scroll == last_reconciled_scroll && !dirty_data` (steady within-row scroll = pure ADR 0007
      transform, 0 rebind, no LAYOUT); on a crossing it recycles leaving hosts to `pool`, pops/
      builds entering hosts, rewrites `row_offset`, and marks the canvas `LAYOUT` — **never
      `STRUCTURE`** (which bubbles) — so relayout stays contained to the canvas subtree.
      `absorb_measurements` (after relayout) feeds measured heights back into the height tree and
      sets `dirty_data` for anchor renormalization (variable-height scroll correction). Recycle
      API: `NodeArena::detach_child` (unlink, keep slot valid+parked), `NodeStore::free_subtree`
      (post-order free descendants only, `effects.cancel_for_node` per node so scoped effects
      clean up on unmount), `BuildCx::with_parent` (author a body under a host). Public
      `VirtualListStyle` + `BuildCx::virtual_list(style, item_count, item_fn)` mounts no items at
      build; logical index = identity this slice (`key_of` hook reserved for Phase 7 reorder).
      `AppDriver` gained a `virtual_lists` field, reset in `on_launch`, reconciled each Layout
      phase. Measured (§7.3 / §36): release `crates/ui/benches/large_list.rs` over a 100k-row list
      — steady within-row frame ≈ 357 ns, boundary-crossing frame ≈ 5.4 µs; the bench's startup
      assertion proves the steady path rebinds 0 rows and grows no reused scratch.
      Verified: `cargo xtask check-deps` (13 crates; criterion is a dev-dep, not a DAG edge), fmt,
      clippy `-D warnings`, full `cargo test` (121 ui unit tests incl. height-tree round-trips,
      the Appendix C mount-window assertion, steady 0-churn, 3-in/3-out recycle with pool
      conserved, variable-height anchor preservation, scroll-to-end clamp, item-count growth, the
      allocation guard, and effect-cleanup-on-unmount; 3 facade `virtual_list_seam` integration
      tests). No shader change, so no Metal validation needed.
- [x] Slice J — Grid (§69 item 11). DONE (full version: Fixed/Fr/Auto/Percent tracks, spanning,
      auto-flow + explicit placement). ADR: `docs/adr/0009-grid-layout-track-sizing-and-placement.md`.
      As built: the reference framework has no true grid, so Grid is designed from first principles —
      only the re-normalizing Fr free-space sweep is reused. New `LayoutInput::Grid { column_count,
      row_count, column_gap, row_gap, padding, auto_rows, size }` carries only `Copy` scalars; the
      variable-length column/row `TrackSizing` templates + per-child `GridPlacement` ride in two
      `NodeStore` warm side-columns (`grid_tracks: Vec<Option<Box<GridTracks>>>`, `grid_placement:
      Vec<GridPlacement>`), read via `LayoutTree::{grid_column_tracks, grid_row_tracks,
      grid_placement}` hooks — so `LayoutInput` stays `Copy` and the pass stays registry-free (same
      pattern as Scroll/VirtualList). `TrackSizing { Fixed(f32), Fr(f32), Auto, Percent(f32) }` is a
      deliberately separate enum from `Length` (a track slot resolved against the grid content extent,
      with `Percent` having no `Length` analogue) so no Flex/Leaf/Scroll/AbsoluteRows match site
      reasons about grid semantics. `crates/ui/src/grid.rs`: `place_children` (explicit-first, then
      auto-flow row-major over a `Vec<u64>` occupied bitset, implicit rows via `auto_rows`, returns
      row count) and `solve_tracks` (pass 1 Fixed/Percent/Auto; pass 2 re-normalizing Fr sweep). The
      `layout_grid` arm in `crates/ui/src/layout.rs` places children, solves both axes, computes cell
      rects by `prefix_offsets`/`span_end` (interior span gaps belong to the cell, trailing gap does
      not), and lays each child into its cell honoring its own `Size` (a `Fill` child stretches, a
      `Fixed`/`Fit` child hugs — so a nested `flex` composes inside a cell). Authoring API:
      `BuildCx::grid(style, children)` + `BuildCx::place(placement)` (pins the next child, cleared
      after use, never leaks to a sibling). Facade re-exports `GridStyle`/`TrackSizing`/`GridPlacement`
      via `viso_ui`; `grid_seam` guards the surface. Measured (§7.3 / §36): release
      `crates/ui/benches/grid_layout.rs` — `grid_relayout_12x20` (240-cell Fr grid, full re-layout)
      ≈ 8.33 µs median (`[8.13, 8.33, 8.51] µs`); the bench asserts scratch capacity does not grow
      across frames.
      Verified: `cargo xtask check-deps` (13 crates), fmt, clippy `-D warnings`, full `cargo test`
      (grid.rs 11 unit tests — placement defaults, auto-flow wrap, explicit routing, span-2, and the
      6 solver cases; layout.rs 6 arm tests incl. `a_two_by_two_fr_grid_places_children_in_cells`,
      `gap_and_padding_offset_cells_and_shrink_free_space`, `a_fit_child_hugs_its_content_within_the_cell`,
      `adding_children_creates_implicit_rows`, `repeated_layout_of_a_stable_grid_grows_no_scratch`;
      `build_cx_grid_places_an_explicit_child`; facade `grid_seam`). No shader change, so no Metal
      validation needed.
      Deferred (per ADR 0009): `minmax`/`repeat`/`fit-content`, named lines/template-areas, subgrid,
      baseline alignment, spanning-item contribution to Auto track sizing, per-grid-node `GridScratch`
      hoisting, and Adaptive (the other half of §69 item 11) — each its own later slice.

---

## Open design note — driver → scheduler StateDirty channel

`RuntimeCx` currently exposes only `request_redraw(window)` (a platform beat), not
`add(RedrawReason::StateDirty)`. The scheduler adds reasons itself from raw events;
a `RedrawRequested` beat drains whatever reasons are pending. Cleanest fit for B2:
have the facade's first write call `request_redraw` (the beat) and add a narrow
`RuntimeCx` method to record `StateDirty` so the scheduler's decide/idle bookkeeping
stays honest. Decide the exact seam when implementing B2, keeping the zero-CPU-when-idle
contract intact.
