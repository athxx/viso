# Viso — TODO

> Working memory so progress survives interruption. Update as sections land.
> Authority order: accepted ADR > architecture doc > AGENTS.md > crate docs > convention.
> Standing constraints (do not drop):
>
> - No Makepad-keyword comments in code. No `§`+number section symbols in code/comments; strip any you pass.
> - Reference `/Users/x/code/vizo/makepad` code when designing a subsystem; don't invent alone.
> - Performance-first: prefer SIMD + zerocopy; unsafe allowed (with `SAFETY:` note).
> - Commit per section; keep changes focused.
> - Verify: `cargo xtask check-deps` (stays 15 crates) + build + clippy `-D warnings` + fmt + test; headless integration where UI; real-machine Metal when shaders change.
> - `viso-math` + `viso-ende` landed as Tier-A leaf foundations (ADR 0012). Both are DAG leaves
>   (empty allowed-edges, no third-party deps). No consumer wired yet: migrating the existing
>   f32 geometry (`viso_render::{Rect,Point}`, `viso_ui::Vec2`) onto `viso-math`, and wiring
>   `viso-ende`'s advanced transport / schema-registry / Studio-Inspector protocol + cache/snapshot,
>   are separate later tasks (Phase 9+ for the ende consumers). Until those land the crates just
>   exist as owned foundations. Crate count is now 15.

---

## Done (Phases 3–5 + hard gaps) — summary

Detail lives in the ADRs (`docs/adr/`) and the architecture doc's "As built" notes; this
is the ledger, not the design record. All landed, verified, committed.

- **Phase 3 — retained tree + reactive state.**
  - Slice A: `Component`/`BuildCx`, `NodeStore` SoA, generational `NodeArena`; two-pass Flex
    measure/layout; `paint_tree`; facade drives the retained tree through frame phases. ADR 0003.
  - Slice B: reactive state + per-class layered dirty propagation + incremental recompute.
    B1 dirty propagation (paint-only never bubbles layout); B2 `State`/`StateStore`/
    `BindingTable`/per-frame transactions; B3 memo-gated `Computed` + scoped `Effect`, each on
    its own reverse index. ADR 0005. Satisfies **doc Phase 5 (Reactive)** exit in full.
- **Phase 4 — Input / Style / Semantics / layout containers.**
  - Slice C: world bounds + hit test (`Rect::contains`, `hittable` column, `HitTestTree`,
    reverse-order targeted descent).
  - Slice D: pointer routing (normalize → capture → target → bubble), cold per-node handler
    column, facade `on_input` with reused ancestry buffer. ADR 0006 (Application scene seam,
    `BuildCx::with_reactive`, `EventCx::pointer()`).
  - Slice E: keyboard / focus / IME (focus slot + `focusable` ring `focus_next`, `KeyRouter`
    `route_key`/`route_ime` over the shared `dispatch_chain`).
  - Slice F: style token (`TokenId`/`StyleId` interning, `Theme` → `StateStore` cell, incremental
    `resolve_styles`, clean frame runs no cascade).
  - Slice G: semantics (`SemanticsTree` derived from the Node model, incremental via SEMANTICS
    dirty class, default `Button` role for interactive nodes).
  - Wrap-up: `01-counter` interactive example end to end.
  - Slice H: Scroll — `LayoutInput::Scroll`, scroll/world/content/capture columns, `scroll_by`
    (TRANSFORM|HIT_TEST|PAINT, no LAYOUT), `resolve_transforms`, clip layers, `ScrollRouter`.
    ADR 0007.
  - Slice I: VirtualList — Scroll viewport over an `AbsoluteRows` canvas; `HeightTree` (Fenwick),
    `HeightCache`, `reconcile`/recycle/pool, steady within-row scroll = pure transform (0 rebind).
    ADR 0008.
  - Slice J: Grid — Fixed/Fr/Auto/Percent tracks, spanning, auto-flow + explicit placement;
    `place_children` + `solve_tracks`; `BuildCx::grid`/`place`. ADR 0009.
- **Hard gaps (this session) — closed before DSL.**
  - #1 macOS native input (commit cf76a17).
  - #2 hot-path 0-alloc: Flex/Grid layout (f64ea62); renderer `encode` borrow-free
    (`InlineUniforms` by value + range-based `RenderPass`, scratch reused via `mem::take`→
    `clear`→refill→put-back) + steady-state allocation-guard bench (43ca0d2).
  - #3 100k-node NodeArena create/traverse/remove benchmark (6a14401) — doc Phase 3/§68 exit.

---

## Deferred / not-yet-built backlog

Recorded when the slice landed; each is "lands when a consumer needs it," not forgotten.
Pull the relevant item into a slice when its trigger arrives.

**Reactive (Phase 5 leftovers).**

- **State inspector** (`StateId -> bindings`, dep-edge introspection). Deferred to **after
  Phase 6** per the current plan — it is a Studio/tooling client (§34/§62), not a runtime hot
  path. Revisit alongside Phase 9 Studio.
- **static / mixed / dynamic reactive benchmark** (§70 exit + §10.3): a baseline proving a
  typed binding never silently falls back to the dynamic path, with `static_binding_eval` /
  `dynamic_binding_eval` / `dynamic_subscribe` / `dynamic_fallback_nodes` counters. No bench
  file yet. Lands when the dynamic fallback path itself is exercised (a `.vs` `dynamic` escape
  hatch in Phase 6) — pairing the benchmark with a real dynamic consumer.

**Input (Slices C/D/E).**

- **World/transform column + clip folding** (Slice C): a distinct world rect recomputed on
  HIT_TEST/TRANSFORM/LAYOUT dirt (Scroll already added `world`; generalize + fold clip down the
  hit-test descent). Cases: translated subtree hit-tests at world position; clip rect excludes
  outside points.
- **`stop_propagation`** (Slices D/E — partially landed in Slice H's `Dispatched { ran, stop }`;
  verify the pointer/key/IME chain all honor it and add the swallow test with the first widget
  that must consume an event, e.g. modal backdrop / button).
- **Pointer capture / drag** — cross-frame held target (Slice H landed the capture holder for
  scroll; generalize to a draggable control).
- **Focus on pointer-down** — click-to-focus; today focus moves only via Tab / programmatic
  request. Lands with the first click-to-focus control.
- **hover / enter / leave** — pointer-tracking state to synthesize enter/leave from moves. Lands
  with hover styling / cursor feedback.
- **Semantics of focus** — accessibility focus / SEMANTICS on focus change (today focus change
  is PAINT-only). Lands with the platform AT bridge.

**Style / Semantics (Slices F/G).**

- **Non-color/radius tokens** — `border` + `spacing`/`typography`/`elevation`/`motion`
  namespaces stay literal until a consumer (bordered widget, text control, shadow, animator).
- **Style version / measure-affecting tokens** — a tokenized field feeding MEASURE must dirty
  MEASURE+LAYOUT, not just STYLE+PAINT; per-field binding class lands with the first such token.
- **Cached bound-node list for `resolve_styles`** — today scans all nodes for the STYLE mark;
  cache when a huge tree makes the scan hot (measured, not now).
- **Per-subtree incremental semantics** — whole tree re-derives on any SEMANTICS dirt; cached
  previous tree + per-subtree rebuild lands when a large tree makes the walk hot.
- **Text-node label invalidation** — a label on a text node is MEASURE+LAYOUT+PAINT+SEMANTICS;
  the MEASURE/LAYOUT classes join `set_semantics` when the first text control lands.
- **Richer roles / state** — `checked`/`expanded`/`value`/`range` grow with the widgets.
- **Platform AT bridge** — feeding the OS a11y API from `SemanticsTree` (platform-tier, no
  `accesskit` dep in the ui tier).

**Layout containers (Slices H/I/J).**

- **VirtualList `key_of` reorder** — logical index = identity today; stable-key reorder reserved
  for Phase 7.
- **Grid advanced** (per ADR 0009): `minmax`/`repeat`/`fit-content`, named lines/template-areas,
  subgrid, baseline alignment, spanning-item contribution to Auto sizing, per-node `GridScratch`
  hoisting, and **Adaptive** (the other half of doc §69 item 11).

**Open design note — driver → scheduler StateDirty channel.** `RuntimeCx` exposes
`request_redraw`/`request_state_flush`; if a cleaner narrow `StateDirty` record is wanted so the
scheduler's decide/idle bookkeeping stays honest, decide the exact seam then, keeping the
zero-CPU-when-idle contract intact.

---

## Phase 6 — Viso DSL: `.vs` (doc §71)

> Goal: a typed, incremental, AOT-friendly external DSL. Pipeline (doc §71 / AGENTS §21.2):
> `.vs source → Streaming Tokenizer → Lossless CST → AST → Name Resolution → Typed HIR →
> UI IR / Binding IR / Shader IR → dev hot reload OR release AOT package`.
>
> **Before each slice:** read `Viso_DSL_1.0.md` + the Makepad `script_mod!`/`ScriptVm` source
> as a *semantics/migration* reference only; design on Viso's typed HIR + retained-tree
> contracts. `viso-dsl` depends on schemas/UI interfaces, never the reverse (§21.1) — the pure
> Rust UI path must keep working with no DSL compiler present. Commit per slice; verify each.
>
> Scope discipline (§21.5.1): ship Core authoring first (component/input/state/computed/
> action/view, node/property/event/control-flow/keyed-list, basic functions). Standard
> (effect/task/resource/slot/style/theme) and Advanced (user traits/generics/metaprogramming)
> come later and must not block the first vertical slice.

Proposed slice order (refine when the first slice lands; each is its own ADR if it touches
DSL language/module semantics, §68):

- [x] **Slice K — frontend lexical layer: Tokenizer + Lossless CST + coarse parser skeleton.**
      DONE (commit 46ea921, ADR 0010). New `crates/dsl/src/syntax/`: streaming tokenizer with all
      spec disambiguation (`%` unit-vs-modulo, `1..2` range, raw-string hash matching, numeric
      separators, escape bounds; never panics / always makes forward progress); rowan-style `Rc`
      green tree (GreenNode/GreenToken/GreenBuilder, ErrorNode + MissingToken, `root.text()==source`);
      byte-primary TextRange/TextSize spans + on-demand LineIndex for scalar/UTF-16 columns; single-
      edit incremental re-lex == full re-lex; coarse recursive-descent parser (Item/Block nodes,
      ErrorNode + synchronize to `;`/`,`/`}`/decl-keyword, multi-error per pass, codes Parse0001-3).
      18 lexer + 8 parser tests (positive, disambiguation, malformed recovery, losslessness,
      incremental, never-hang/panic fuzz). 13 crates, no new deps. NOTE: the *typed grammar*
      (`:` vs `=` context split per §21.5.2, `node name: Type {}` identity, precedence-correct
      expressions) is Slice L — K only groups tokens at declaration/brace granularity.
- [x] **Slice L — typed AST + Name Resolution + module graph.**
      DONE (ADR 0011). Upgraded the coarse Slice-K parser to an event-driven recursive-descent +
      Pratt typed parser (`syntax/grammar/`, event buffer + `build_tree`, precedence-correct
      expressions, `:` property-binding vs `=` assignment split per §21.5.2, `node name: Type {}`
      identity, `for..key`/`on {}`/`child` grammar rules), keeping losslessness + total recovery;
      the coarse parser is retained for the shared `Parse`/`ParseErrorKind` types. Full rust-analyzer
      red tree (`syntax/red.rs`: cached `Rc` `SyntaxNode`/`SyntaxToken`/`SyntaxElement`, parent/offset/
      index identity, bidirectional + ancestor/descendant navigation). Typed AST as red-tree views
      (`ast/`, `AstNode::cast`, no owned duplication). Identity per §10.4: `NameId`/`NameInterner`,
      128-bit `SymbolId` from a self-owned fixed FNV-1a-128 (versioned, known-answer-pinned; no
      `DefaultHasher`/process-seed/byte-offset identity). Deterministic `ModuleGraph::build` from an
      in-memory `SourceUnit` set (sorted by module-path text, not registration order — §21.4), import
      edges, cycle detection (E2003) + ambiguity (E2002). Full cross-module resolver (`resolve/`):
      per-module symbol tables → `SymbolId` + `export` visibility, import alias/selective import,
      cross-module symbol resolution (unresolved E2001), slot-based local scopes with Value/Type/Event
      namespaces (§40), view-local `node`/`for` bindings, `let`/param scopes; resolution only (types
      are Slice M). Unified diagnostics (`diag.rs`): one shared `Diagnostic { severity, code, primary,
      related, notes, message }` + `Severity`, the three `*Kind` enums kept as the single code/message
      vocabulary with uniform `to_diagnostic()`, wrapper structs `ParseError`/`ResolveError` deleted,
      all accumulators `Vec<Diagnostic>`. 93 tests (23 unit + 7 ast + 11 decl + 15 expr + 11 view +
      18 lexer + 8 coarse-parser). 13 crates, no new deps.
      DEFERRED to their consumer slice (parsed to placeholder AST now, no resolution): Advanced
      productions — `trait`/`impl`, general/const generics, `template`/`part`, `style`/`theme`,
      `shader`, `native` schema. Type/effect/capability checking is Slice M.
      SPEC BUGS flagged in ADR 0011 for the owner: (1) §54/§56/§65/§E.1 show property binding with
      `=`, contradicting Appendix A's `:` (colon is authoritative); (2) §21.5.2's `color: theme.…`
      example uses reserved keyword `theme` as a value-path head with no carve-out.

- **Tier-A owned foundations — `viso-math` + `viso-ende` (ADR 0012).** Two dependency-free DAG
  leaves, self-built per the Ownership Ladder. No consumer wired yet (migration onto them is a
  separate later task). Crate count 13 → 15; both registered in xtask with empty allowed-edges.
  - `viso-math`: f32-primary vec/mat/quat/transform/rect/geom (`Vec2/3/4`, `Mat2/3/4` uniform flat
    `[f32;N]`, `Quat`, `Affine2` + `Insets` [reference gaps], `Transform3`, `Point/Size/Rect/Insets`,
    `Ray/Plane/Aabb`) with f64 `DVec2`/`DPoint`/`DRect` scoped to the UI accuracy path. Divergences:
    methods not assoc fns; **half-open `Rect::contains`** (strict `intersects`) vs **inclusive
    `Aabb`** — deliberate contrast. Internal cfg-gated Mat4 SIMD (SSE2/NEON/wasm128 + scalar),
    bit-exact vs scalar (verified aarch64/NEON). `#[repr(C)]`+Copy, no usize/String/dyn/heap/serde
    on public types. 61 tests + 6 release benches.
  - `viso-ende`: bounded decoder (single `read_raw` gate, never panics / never over-reads —
    20k-iter fuzz smoke), mirrored LE + LEB128-varint + zig-zag codec, heap-free `Copy`
    `DecodeError`, `WireId`/`ProtocolTag` (format not identity → stays a leaf), hand-rolled JSON
    emitter. No serde, no RON, no media codecs (serde compat → `integrations/serde`). 16 tests.
    DEFERRED to Phase 9 consumers: advanced transport / schema-registry / Studio-Inspector protocol
    + cache/snapshot wiring.
- [x] **Slice M — Typed HIR: schema + type/effect/capability checking.**
      DONE (ADR 0013). Self-built static Typed HIR layer (`hir/`) that re-walks each resolved
      module's AST behind the resolver's module-path→`CompilationUnit` matching, consumes `refs`
      + cross-module `table`s, and emits typed HIR carrying the §116 eight-field node contract
      (resolved symbol / inferred type / effect class / capability set / ownership mode /
      reactive reads / source origin / constant value), with a debug HIR-complete assertion that
      rejects any undetermined type residue (`InferInt`/`InferFloat`/`Unknown`) the source did
      not annotate. Three static checks: (1) TYPE (`ty`+`infer`) — fixed scalar list (no
      `Int`/`UInt`/`Float`/platform-width int, §73), literals type at the expected type under
      context else host default (§75), implicit widening same-family upward only (§76); `Float`
      annotation E2101, illegal implicit conversion E2102, mismatch / non-unique / out-of-range
      E2103. (2) EFFECT (`effect`) — `Pure`/`Read`/`Action`/`Task` × the §81 call matrix per
      `BodyContext`; matrix violation E2501, side effect in a reactive View/Computed E2502.
      (3) STATE/COMPUTED (`component`) — state init reads source-preceding state only (forward
      read E2104, no exception), omitted private `state`/`computed` types infer to a unique
      concrete type (else compile error), all `computed` topologically sorted with cycle path in
      related spans E2105. (4) CAPABILITY (`capability`) — deterministic `BTreeSet`, inferred =
      union of direct conferrals + transitive callee sets via a fixed-point index-based call
      graph; `requires {}` is a public upper-bound contract (inferred must be a subset, else
      E2601). Checks decoupled via `&self` env traits; `lower(graph, units, resolved, interner,
      package) → LoweredPackage` ties it together with one `ModuleEnv` per module (per-component
      symbol focus via a `Cell`). 160 tests (90 unit + 70 Slice-L integration). 15 crates, no new
      deps; no dependency on `viso-widgets`.
      DEFERRED to their consumer slice (lowered to placeholder now — source origin + symbol
      recorded, no deep inference/monomorphization/effect refinement): `system` declarations
      (system hooks / scheduler schema), module-level `fn`/`action`/`task`, `trait`/`impl`,
      generic arity, native schema (the eventual source of capability conferrals + property/event
      schema — this slice infers capabilities from the call graph and uses each component's own
      declarations as its schema), shader, resource, task-async.
- [x] **Slice N — UI IR + Binding IR + the `ui!` proc-macro (lower to the retained tree).**
      DONE (ADR 0014). Lowered Typed HIR → UI IR (static templates + retained-node instantiation,
      NOT a per-frame rebuild, §59) + Binding IR (compiled `StateId -> (node, class)` edges feeding
      the existing `BindingTable` static fast path, §10.2), in a new independent `crates/dsl/src/ir/`
      pass that does not touch the frozen Slice M `ComponentSchema` contract. Built a real `ui!`
      proc-macro in a new `viso-ui-macros` crate (option C, the one crate-count exception 15→16):
      it runs the shared frontend at Rust compile time and emits static `viso_ui` builder tokens —
      no runtime parse, no VDOM. A compiler-known typed binding never silently falls back; `dynamic`
      is the explicit escape hatch that trips `dynamic_fallback_nodes` (§10.3). Real property→
      DirtyClass table (§11) in `ir/dirty_map.rs`. Keyed lists get stable keys, keyless stateful
      repeats flagged (§21.8). The four reactive counters live on `BindingTable`. The deferred
      **static/mixed/dynamic reactive benchmark** landed (`crates/ui/benches/reactive_binding.rs`).
      Control-flow region reconciliation, TwoWayBinding deep semantics, slot/style/theme source
      origin, native-schema property/event validation, and FillClause full semantics are recorded
      structurally and DEFERRED to their consumer slice (the emitter surfaces an explicit
      `compile_error!` for a control-flow region rather than mount it wrong). `component!`/`view!`
      reuse the pass + emitter; only `ui!` shipped this slice.
- [x] **Slice O — dev hot reload (transactional).**
      DONE (ADR 0015). Hot reload is a transaction, not a rebuild (§42/§21.7): the entry
      `hot_reload` runs `plan → diff → migrate → commit`, where the three fallible stages are pure
      functions producing plain data and only `commit` touches the live tree. So a compile/validate
      failure short-circuits at `plan(source)?` **before** commit and the live tree stays at
      last-good — keep-last-good is an invariant of the pipeline shape, no snapshot/rollback (§19/
      §30). Structure changes apply as a directed minimal `StructuralPatch` keyed by the Slice N
      pre-order `NodeKey` numbering (same identity → reuse live instance; type change → rebuild);
      state migrates by durable `SymbolId` identity so editing one line never disturbs another cell;
      focus/scroll survive iff their slot is kept, and what cannot survive is *reported*, not
      silently dropped (§52/§71 exit). The engine lives in `crates/dsl/src/hotreload/` inside the
      existing `viso-dsl → viso-ui` edge — no new crate/edge, still 16 crates; `viso-ui` gained a
      `#[repr(C)]` `StateKey` (layout twin of `SymbolId`) plus `migrate_state` / `set_scroll` /
      `clear_static`/`rebuild_static`, importing nothing from `viso-dsl` (§21.1). Takes the
      reference framework's live-editing *semantics* (template-is-truth, same-identity-reuse /
      type-change-rebuild) and exceeds them into a full atomic transaction with explicit identity-
      keyed migration (§38.4). This slice commits the static-node subset (single-root flex/grid/
      scroll/leaf); a control-flow region is rejected before commit as in Slice N. Per-slot instance
      reuse across a *structural* edit, `@migrate(from:)` + value-level safe widening, and
      `component!`/`view!` reload entries are recorded and DEFERRED.
- [ ] **Slice P — release AOT package.**
      Release build emits compact typed IR/assets; no `.vs` parse at startup (§21.6). Dev metadata
      (source maps, inspector strings) stripped from the steady-state release path (§60). Tests:
      an AOT-packaged app boots + renders with the DSL compiler absent from the release graph.
- [ ] **Slice Q — Shader IR (if in scope this phase, else defer to a shader slice).**
      `shader` surface → typed IR → strict layout validation → backend codegen (§19); hot-reload
      shader error keeps last-good pipeline. May split to its own phase; keep it out of the first
      DSL vertical slice unless a consumer needs it.

**Phase 6 exit criteria (doc §71):** formatter/LSP/goto/rename/reference usable; release needs
no startup `.vs` parse; hot reload is compile → validate → atomic patch; a failed compile keeps
last-good UI; state/focus/scroll migration has explicit rules.

**Deferred past Phase 6 (doc §72+):** Phase 7 native widget rewrites (Tier 1–6); Phase 8
platform services / async / app framework; Phase 9 Studio / Inspector / CLI (the deferred state
inspector rejoins here); Phase 10 `viso migrate` source-level Makepad migration tooling +
isolation finish. These are out of scope until Phase 6 lands.
