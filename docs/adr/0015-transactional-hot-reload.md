# ADR 0015 — Transactional hot reload (compile → diff → migrate → commit)

- Status: Accepted
- Date: 2026-09-04

## Context

Slice N (ADR 0014) turned a declarative view into a static UI IR template + compiled
Binding IR and mounted it once through `viso_ui` builder calls — no per-frame rebuild.
But it mounted a template *once*. There was no path from an *edited* fragment to a live
change: no diff of old vs new template, no migration of running state / focus / scroll,
no atomic apply, and no keep-last-good on a bad edit. `todo.md:249`'s Slice O closes
that gap for the dev live-editing loop.

The governing rule is architecture section 42 / AGENTS 21.7: **hot reload is a
transaction, not a rebuild.** The ordered pipeline is

```text
compile candidate (plan) → structural diff (diff) → state/focus/scroll migration (migrate) → atomic commit (commit)
```

and on a compile/validate failure the live UI stays at its last-good state (section 19 /
section 30). A structural change must migrate state/focus/scroll by explicit rules
(Phase 6 exit, section 71; checklist section 52).

The reference framework's live-editing model is the migration baseline: "the new
template is truth, over-apply field-for-field," with same-type-id → reuse the instance
(so its state/focus/scroll/animation survive) and type-changed → rebuild. It has **no
explicit structural diff, no tree-side transaction, and no explicit focus/scroll
restore** — only its *file* layer does validate-then-commit for keep-last-good. Per
section 38.4 (migrate semantics, not architecture) Viso **takes the semantics** —
template-is-truth, same-identity-reuse / type-change-rebuild — and **exceeds them**: a
full atomic transaction with explicit identity-keyed migration of state, focus, and
scroll.

Two design choices were confirmed before implementation:

1. **State migrates by `SymbolId` stable identity**, not by position — so editing one
   line leaves every other reactive cell untouched.
2. **Structure changes apply as a directed minimal diff** keyed by the same template
   pre-order `NodeKey` numbering Slice N's Binding IR uses — so a kept slot reuses its
   live instance (state/focus/scroll/animation ride along) without a runtime search.

This slice fires the section-68 ADR triggers for **reactive semantics** and **Viso DSL
language/module semantics** (hot-reload state-migration behavior). It touches no crate
dependency direction — the engine lands inside the existing `viso-dsl → viso-ui` edge.

## Decision

### 1. The transaction engine lives in `viso-dsl`; no new crate, no new edge

The engine drives the shared frontend (`plan` recompiles through the same
`lower`/`lower_view_block` path `ui!`/`component!`/`view!` use) and holds the compile-
stable `SymbolId` / `NodeKey` identities the diff and migration are keyed on, then calls
*down* into `viso_ui` to apply the patch. That is exactly the `viso-dsl → viso-ui`
direction, which already exists. So the engine is a new module directory
`crates/dsl/src/hotreload/` (section 5 — a complex subsystem is a directory), and the
runtime stays **16 crates** with the section-10 DAG unchanged (`viso-ui` never gains a
`→ viso-dsl` edge; section 21.1's pure-Rust-UI rule holds).

The bridge from the compiler's `SymbolId { hi, lo }` to a runtime state cell is a
`#[repr(C)]` `StateKey { hi, lo }` on the `viso-ui` side with the same layout, passed in
by the engine — so `viso-ui` migrates state cells by identity **without importing any
`viso-dsl` type**.

### 2. Every fallible stage is a pure function that runs before the commit

`plan`, `diff`, and `migrate` read their inputs and allocate only their own output
(plain data); none touches the live tree. The single mutating stage, `commit`, runs last.
The entry `hot_reload` is `plan(source)? → diff → migrate → commit`, and the `?` on
`plan` short-circuits with `Err(Vec<Diagnostic>)` **before** commit is reached.

- `plan.rs` — recompile + validate the candidate; any fatal `Diagnostic` (section 30,
  code + span) returns `Err`. Output: `CandidatePlan { tree, bindings, sources,
  source_names }`, with `symbol_for_name` bridging a source name to its `SymbolId`.
- `diff.rs` — align the last-good `UiTree` and the candidate `UiTree` by the shared
  pre-order `NodeKey` (the walk exactly reproduces `lower_bindings` / `analyze_keys`
  numbering, so a diff key names the same runtime node the recompiled binding edges
  target). Output: a directed `StructuralPatch { keep, replace, insert, remove }`.
  Identity is `(type_name, NodeKind)`: same → `keep` (reuse instance); changed →
  `replace` (rebuild, state lost); trailing extra/missing → `insert` / `remove`.
  `is_structure_preserving()` is the property-only fast path (every node kept).
- `migrate.rs` — match reactive sources by `SymbolId` set membership across the two
  templates: in both → `Keep`; candidate-only → `New`; last-good-only → `Dropped`.
  Focus survives iff its slot is in the patch's `keep` set; a scroll offset is
  restorable iff its container's slot is kept. Output: `MigrationPlan { states, scroll,
  focus_survives }` — pure data, applied by nobody here.

### 3. Keep-last-good is an invariant of the pipeline shape, not a snapshot

Because the only mutating stage runs last and after every fallible stage has already
succeeded, a rejected reload **never reaches** `commit`, so the live tree is left exactly
at its last-good state. There is no deep tree copy, no rollback, no restore path to get
wrong — the guarantee falls out of "all fallible work is pure and precedes the single
apply." This mirrors the reference file layer's validate-then-commit while making it a
tree-level property. It is the load-bearing invariant of this slice, recorded here so a
future edit that moves fallible work into `commit` (or makes `commit` fallible) knows it
is breaking the contract.

### 4. `commit.rs` is an infallible applier in a fixed order

`commit(rt, plan, patch, migration, anchors) -> HotReloadReport`, over a `LiveRuntime`
bundle of borrows (`store`, `states`, `bindings`, `effects`, `lists`, `root`, `scratch`)
so it drives from a headless test and allocates nothing beyond the small key-to-node map
a reload needs. Order:

1. **Structural patch.** A structure-preserving edit walks the live tree in the shared
   pre-order and maps each `NodeKey` to the existing `NodeId` — zero teardown, every
   instance (and its state/scroll/focus/animation) reused in place. A non-preserving
   edit rebuilds the template through `build_tree`, the runtime twin of the Slice N
   emitter (same `flex`/`grid`/`scroll`/`leaf` calls in the same order, so runtime
   `NodeKey`s match the binding edges).
2. **State migration** by durable identity: a `Keep` bridges `SymbolId → StateKey` and
   preserves the running value (the widen closure returns the prior value); a `New`
   allocates a fresh cell. Counts land in the report (`migrated` / `reset`).
3. **Rebind**: replace the whole static binding table (`clear_static` + `rebuild_static`)
   with the recompiled edges, mapping each edge's `NodeKey` to its live node and each
   `SymbolId` to its migrated `StateId` — a dense one-shot rebuild keeps the `for_state`
   slices contiguous (section 10.2).
4. **Absolute focus / scroll restore** to the migration-preserved values; a lost focus
   or an unrestorable scroll is *counted in the report*, never silently dropped.
5. **Targeted dirty + flush**: mark exactly the rebound nodes dirty, then
   `flush_state_transactions` over the migrated state set so only changed nodes recompute
   (section 11).

The `HotReloadReport { migrated, reset, focus_lost, scroll_lost }` makes the transaction
introspectable (section 34 / 62).

### 5. Scope: static-node subset; structural rebuild loses per-slot instance reuse (for now)

The commit targets the same static-node subset the Slice N emitter targets: a single-root
template of flex / grid / scroll / leaf. A template carrying a control-flow region
(`if` / `for` / `match`) is rejected before commit — the same boundary the `ui!` emitter
enforces — so the commit never interprets one.

For a **structural** edit this slice rebuilds the whole template (correct + simple) rather
than doing per-slot surgery. State cells still survive by `SymbolId` identity (the state
store is untouched by a node rebuild), and surviving scroll/focus are carried by absolute
restore where their slot is kept — but a rebuild replaces live *instances*, so focus/scroll
that lived on a rebuilt slot are honestly **reported lost**. Per-slot instance reuse across
a structural edit (keeping kept subtrees' live instances while surgically replacing only
changed ones) is a deliberate later refinement, noted here so the tradeoff is on record.

## Consequences

- The workspace stays **16 crates**; `cargo xtask check-deps` reports "16 crates, all
  edges within the section 10 DAG" — the engine adds no crate and no dependency edge
  (it lives inside the existing `viso-dsl → viso-ui` direction; `viso-ui` gains only the
  layout-compatible `StateKey` and the `migrate_state` / `set_scroll` / `rebuild_static`
  primitives, importing nothing from `viso-dsl`).
- Hot reload is proven end to end headless (`crates/dsl/tests/hot_reload.rs`): a valid
  property-only edit atomically patches in place — every kept node keeps its `NodeId`
  (instance reuse) and its reactive cell keeps its running value; an invalid edit is
  rejected before commit, leaving the live tree and every cell field-for-field identical
  to the last-good build (proving the commit was never reached); a structural edit
  migrates a same-`SymbolId` cell's value across the rebuild and reports the focus/scroll
  that could not survive.
- Unit tests sit beside each pure stage: `diff.rs` (NodeKey alignment: keep / replace /
  insert / remove / reorder-as-replace), `migrate.rs` (state keep/new/dropped by identity,
  focus survives iff slot kept, scroll restored only for surviving containers), and the
  `viso-ui` migration/rebind primitives (`migrate_state` keep/widen/reset, `set_scroll`
  clamp, `rebuild_static` slice contiguity).
- Deferred (recorded, not built this slice): per-slot instance reuse across a structural
  edit; control-flow region reconciliation in a reload (rejected before commit, as in
  Slice N); `@migrate(from:)` and value-level safe widening beyond keep-verbatim;
  `component!` / `view!` reload entries (they reuse this engine and the shared frontend).
  The release AOT path (Slice P) and Shader-IR hot reload with last-good pipeline
  preservation (Slice Q, section 19) build on this transaction shape.
- Verification: `cargo build --workspace` / `cargo clippy --workspace --all-targets -D
  warnings` / `cargo fmt --all --check` clean; `cargo test -p viso-dsl -p viso-ui` green
  (pure-stage unit tests, the `viso-ui` migration primitives, and the three headless
  hot-reload integration tests). `cargo xtask check-deps` green (16 crates). This slice is
  pure CPU/logic — no shaders/MSL touched — so no on-device Metal run was required.

## Amendment — 2026-10-09: the plan is lowered on the host; the commit runs in the app

Decision 1 placed the whole transaction in `viso-dsl`. That made a dev artifact link the
`.vs` compiler and re-plan every patch from source, which `Viso_Hot_Reload.md` §9 forbids
("do not ship source diff to runtime"). The pipeline's shape (decisions 2–4) is
unchanged; where its halves run is:

- **Host (`viso-dsl::hotreload::patch`)**: `plan → diff → migrate → retype`, as before,
  then *lowered* to a typed `ReloadPlan` in runtime names — a static node by its
  pre-order static index, a region node by `(region, arm, item)`, a state by its
  durable `StateKey` (the runtime twin of `SymbolId`, decision 1's bridge) with its
  `Keep|Convert|Reset|New` action, initial value, behavior slots and `Retyping`. The
  candidate travels in its release form, the `ViewPackage` a release build embeds.
  Dropped states are omitted.
- **App (`viso-view::dev::commit`)**: decode, verify the behavior module and stage
  (`NACK_UNLOADABLE_VIEW` on failure), then the infallible commit of decision 4, keyed
  only by static index and `StateKey`: no name lookup, no template diff, no source.
  A structure-preserving edit of a view without regions restyles each static node in
  place, marks only edges that were not bound before, and flushes only cells whose
  value changed — so a property edit dirties that property's class on its node alone.

Keep-last-good (decision 3) holds on both sides: a candidate that does not compile never
leaves the host, and one that does not stage is NACKed before any node changes. The
`viso-dsl → viso-view` edge (behind `hot-reload`) carries the patch types to the host;
no artifact links `viso-dsl` (`cargo xtask check-release-absence` asserts it of the dev
artifact).

## Amendment — 2026-10-10: per-node structural ops replace the whole-root rebuild

Decision 5 accepted whole-root rebuild for any structural edit as a deliberate, recorded
tradeoff. `Viso_Hot_Reload.md` §66 and the diff's own `StructuralPatch` (keep / replace /
insert / remove, `NodeKey`-aligned by LCS per parent) always computed the finer-grained
edit; only the lowering to `ReloadPlan` collapsed it to one `preserving: bool`, forcing
`commit.rs::apply_structural`'s non-preserving branch to free the whole view root and
rebuild it from scratch — losing every kept sibling's `NodeId` to an edit that touched one
node, in direct tension with the sentence this decision's own section 5 border lives next
to in `Viso_Hot_Reload.md` §12: a named/stable sibling's state must not be lost just
because an unrelated node was inserted ahead of it.

This is a node-identity change (section 68 ADR trigger), decided for a region-free mount
only — a mount with a control-flow region keeps the whole-root rebuild decision 5 recorded,
unchanged:

- `ReloadPlan` (`crates/view/src/dev/patch.rs`) gains `structural: Vec<StructuralOp>`
  alongside the unchanged `preserving: bool` and `nodes: Vec<NodeCarry>`. `StructuralOp`
  is `Remove { node }` / `Replace { node, start, subtree }` / `Insert { parent, before,
  start, subtree }`, every `NodeRef` naming a node of the *last-good* tree — the one the
  runtime already holds live — so each op resolves independently of the order the runtime
  runs them in (an insert's anchor is always a kept node, whose live id never moves mid-
  batch). `subtree` is the candidate's own `AotNode` pre-order table, sliced at the op's
  static ordinal range — a contiguous subslice of a pre-order, `child_count`-linked table
  is itself a valid standalone tree, so no new encoding was needed. `DEV_PROTOCOL_VERSION`
  moved 5 → 6 for the new wire variant.
- The host (`viso-dsl::hotreload::patch::lower`) computes `StructuralOp`s from the diff's
  own `insert` / `remove` / `replace` lists it already had and previously discarded, gated
  empty — the existing whole-rebuild fallback — whenever either template carries a region;
  `commit.rs::apply_structural` keeps that fallback as its last branch, now reached only
  when `plan.structural` is empty.
- `commit.rs::apply_structural_ops` (the new middle branch, between the structure-
  preserving restyle and the whole-rebuild fallback) drives the arena's existing surgical
  primitives — `free_tree` for a remove, `build_nodes` on an op's own `AotNode` subslice
  plus `arena_insert_before` for an insert, the same pair for a replace's old-subtree-out/
  new-subtree-in — all pre-existing, previously reached only from a region's own fragment
  reconciliation, never from this path. A node no op names is never freed: a `Tabs`
  instance or anything else untouched by the edit keeps its live `NodeId`, its focus, its
  scroll, its edit buffer, with no `NodeCarry` needed for it at all. `NodeCarry` now only
  carries migratable state across a `Replace`'s own boundary — the subtree the op *does*
  rebuild — exactly where decision 5's "a rebuild replaces live instances" still applies.
- `diff.rs` grew what it needed to translate into ops it previously had no reader for:
  `InsertedNode`/`RemovedNode` name only the root of a disjoint subtree (not every
  descendant — a subtree frees or builds as one arena call), carry their candidate-tree
  anchor (`parent`, `before`) so the lowering can resolve it to the last-good node that
  anchor already is, and `KeptNode` gained `under_replace` so a kept node nested inside a
  `Replace`'s own subtree is not also mistaken for a structural op of its own — its state
  still carries, through the `Replace`'s `NodeCarry` entries, but the node itself rebuilds
  with its parent regardless.

Consequences: `crates/dsl/tests/hot_reload.rs` gained `an_inserted_sibling_keeps_every_
other_node_s_live_identity` and `a_removed_sibling_keeps_the_rest_s_live_identity`,
asserting `NodeId` equality (not just carried-value equality) across a structural edit;
two prior tests that asserted the opposite — that an edit elsewhere in the tree rebuilt an
untouched kept node — were corrected to assert the new, intended identity-preserving
behavior instead. The wire round-trip suite (`crates/view/src/dev/wire.rs`) covers all
three `StructuralOp` variants.
