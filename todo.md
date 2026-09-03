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
> - `viso-ende` (doc §9/§10.5) is a planned 14th crate but is **deferred until a consumer needs it**
>   (Studio/Inspector/Hot-Reload transport, cache/snapshot — all Phase 9+). It is NOT built for
>   Phase 6 DSL; the frontend/HIR/UI-IR slices don't touch it. When it lands, bump the check-deps
>   count and add its forbidden edges (§10.1). Until then, 13 crates is correct.

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
- [ ] **Slice M — Typed HIR: schema + type/effect/capability checking.**
      Component/native schema (§21.5.4 — native APIs from generated schema, authors don't
      re-declare signatures); typed properties/events/state; `view` is side-effect-free,
      `effect`/`event` carry effects, `state`/`computed` enter the incremental dep graph;
      `Computed` purity enforced by type. Private `state` may infer type from a stable initializer;
      public/exported boundaries stay explicitly typed (§21.5.3). Capabilities inferred from the
      typed call graph, checked against package/profile grants (§21.5.4, §22). Diagnostics carry
      severity + stable code + primary/related spans + notes + fix suggestions (§30). Tests:
      positive type-check, diagnostic tests, effect/capability violations.
- [ ] **Slice N — UI IR + Binding IR (lower to the retained tree).**
      Lower Typed HIR → UI IR (static templates + retained-node instantiation, NOT a per-frame
      rebuild, §59) + Binding IR (compiled `StateId -> (node, class)` edges feeding the existing
      `BindingTable` static fast path, §10.2). A compiler-known typed binding must NOT silently
      fall back to dynamic tracking; `dynamic` is an explicit escape hatch that trips
      `dynamic_fallback_nodes` (§10.3). This is where the deferred **static/mixed/dynamic reactive
      benchmark** lands (pair it with the first `dynamic` consumer). Keyed lists get stable keys
      (§21.8). Tests: `ui! { ... }` lowers to a retained tree a headless frame renders; a bound
      `set` drives targeted invalidation through the compiled edges; strict typed example emits no
      dynamic fallback.
- [ ] **Slice O — dev hot reload (transactional).**
      compile → validate → prepare migration → diff → commit atomically (§21.7). On compile/
      validate failure keep last-good UI (§19/§30). state/focus/scroll migration rules on a
      structural change (§71 exit, §52). Tests: a valid edit atomically patches the retained tree;
      an invalid edit keeps last-good; a structural edit migrates state/focus/scroll per the rule.
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
