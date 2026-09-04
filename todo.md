# Viso — TODO

> Working memory so progress survives interruption. Update as sections land.
> Authority order: accepted ADR > architecture doc > AGENTS.md > crate docs > convention.
> Standing constraints (do not drop):
>
> - No Makepad-keyword comments in code. No `§`+number section symbols in code/comments; strip any you pass.
> - Reference `/Users/x/code/vizo/makepad` code when designing a subsystem; don't invent alone.
> - Performance-first: prefer SIMD + zerocopy; unsafe allowed (with `SAFETY:` note).
> - Commit per section; keep changes focused.
> - Verify: `cargo xtask check-deps` (now 17 crates) + build + clippy `-D warnings` + fmt + test; headless integration where UI; real-machine Metal when shaders change.
> - `viso-math` + `viso-ende` landed as Tier-A leaf foundations (ADR 0012). Both are DAG leaves
>   (empty allowed-edges, no third-party deps). No consumer wired yet: migrating the existing
>   f32 geometry (`viso_render::{Rect,Point}`, `viso_ui::Vec2`) onto `viso-math`, and wiring
>   `viso-ende`'s advanced transport / schema-registry / Studio-Inspector protocol + cache/snapshot,
>   are separate later tasks (Phase 9+ for the ende consumers). Until those land the crates just
>   exist as owned foundations.
> - Crate count is now 17 (added `viso-ui-macros` in Slice N, `viso-lsp` in Slice R).

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
- [x] **Slice P — release AOT package.**
      DONE (ADR 0016). The third lowering target of the one shared frontend (after Slice N's builder
      tokens and Slice O's live commit): `build_package(source)` runs `plan` and serializes the
      static template into a compact `viso-ende`-framed blob, and a release app instantiates it with
      **no `.vs` parse at startup** (§21.6). The exit criterion is a dependency-graph fact — the
      release load path must not reach `viso-dsl` — so the package types + loader live in **viso-ui**
      (`crates/ui/src/aot/`, release-path resident, zero `viso-dsl` dep) while the build-time emitter
      lives in **viso-dsl** (`crates/dsl/src/aot.rs`); the wire format has a single source (the
      `Encode`/`Decode` impls are viso-ui-side, the emitter just constructs that type and encodes).
      The package binds by durable `StateKey` identity (`from_parts(hi, lo)`, the `SymbolId` layout
      twin) and references nodes by pre-order `NodeKey` index, so property names / type names /
      local names / spans are stripped from the steady-state release path (§60). `viso-ende` frames
      it with a bounded, panic-free decode — the safety precondition for loading an untrusted asset
      (§30). Two `→ viso-ende` **leaf** edges keep the DAG at 16 crates; the load path imports
      nothing from `viso-dsl`, so the exit criterion holds at the type-system level. Proven end to
      end headless (`crates/dsl/tests/aot_package.rs`): a packaged app boots + renders with the
      compiler absent from the load path, a corrupt blob is a decode error not a panic, and the
      AOT-loaded tree is structurally identical to the Slice O live-commit tree from one source
      (the three targets are one frontend). Static-node subset only; a control-flow region is
      rejected before packaging, as in Slice N/O. Shader-blob AOT, the "generated Rust data" variant,
      control-flow AOT, and `component!`/`view!` AOT entries are recorded and DEFERRED.
  - **Deferred from Slice P (recorded, not swallowed):**
    - Shader-blob AOT packaging — §41 lists "Shader blobs" in the release output, but Slice P's exit
      criterion is the UI IR loop; this is Slice Q-adjacent (shader hot reload) and waits for it.
    - The "generated Rust data" package variant (§41's "embedded asset *or* generated Rust data").
      Slice P shipped the embedded-asset blob path (single format source, `viso-ende`-framed); the
      Rust-const codegen variant is a later option that would reuse the same emitter output.
    - Control-flow (`if`/`for`/`match`) AOT — the same static-node-subset boundary as Slice N/O;
      rejected at `plan` before packaging until a consumer slice adds control-flow lowering.
    - `component!`/`view!` AOT entry points — they reuse this emitter + the shared frontend; only
      the fragment source path was wired this slice.
- [x] **Slice Q — Shader IR.**
      DONE (ADR 0017). A real typed shader IR (`crates/shader/src/ir/`) is now the single source of
      truth: one `ShaderIr` per built-in emits **both** the MSL `InstanceIn`/`VertexIn` struct
      (`emit_msl`) **and** the validated `InstanceSchema` (`emit_schema_attrs`), so the `msl.rs`
      hand-written duplication — the "implicit shader instance field-order ABI" §56 targets — is
      gone; a strengthened three-leg test proves MSL struct order == schema order == IR attribute
      order cannot drift. The four `quad_ir`/`image_ir`/`glyphrun_ir`/`mesh_ir` constructors are the
      only remaining hand-written field contract; the `*_MSL()`/`*_schema()` re-exports keep their
      `&'static` contract via a cold `OnceLock` cache, so `viso-render` is unchanged. Added the
      §36.1 explicit CPU↔GPU cross-check: `InstanceLayout::validate_against` now compares byte offset
      and stride (not just count/name/format) against the `#[repr(C)]` `offset_of!` truth, with new
      `LayoutError::{OffsetMismatch, StrideMismatch}` — turning the silent-memory-corruption gap
      Makepad leaves into a registration-time error (§30/53). A VM-free last-good holder
      (`ShaderPipeline`, `crates/shader/src/reload.rs`) compiles → validates → replaces `last_good`
      atomically only on success; a failed ABI reload returns `Vec<Diagnostic>` and leaves
      `last_good` byte-for-byte intact (§19 keep-last-good, structurally the DSL `hotreload`
      invariant but independent, no shared VM). Diagnostics are self-contained (`crates/shader/
      src/diag.rs`): a `Severity`/`Diagnostic` parallel to the DSL shape but keyed by `CompileStage`
      (no text span yet), because `viso-shader` is below `viso-dsl` in the DAG and cannot import its
      `Diagnostic`. Codegen is **MSL only**; the built-in bodies are byte-identical to the prior
      hand-written MSL (asserted by derivation tests), so no on-device Metal run was required. No
      crate, no new DAG edge (16 crates); all work inside the existing `viso-shader → viso-gpu` edge.
      Proven headless: the `viso-render` golden test passes (all four real built-in layouts clear the
      new offset/stride guard at registration), the reload tests prove keep-last-good field for
      field, and `viso-ende` is unaffected.
  - **Deferred from Slice Q (recorded, not swallowed):**
    - Shader text frontend (source → token → CST parser) — the built-ins use a Rust-side structured
      IR builder; a text path would prematurely duplicate the `viso-dsl` frontend and §36 does not
      require built-ins to travel it. When it lands, `Diagnostic` gains a primary source span.
    - HLSL / SPIR-V / WGSL backend codegen — §36's "broader/deferred targets"; this slice emits MSL.
    - Shader-blob AOT packaging (the Slice P deferred item, §41) — there is now an IR to serialize,
      but it is not in this slice's exit criterion.
    - Body expression-level shader AST / swizzle type system — the built-in bodies use existing MSL
      fragments; the full swizzle/function type system is only needed once users author shader logic
      rather than the built-in primitives.

- [x] **Slice R — formatter + LSP.**
      DONE (ADR 0018). Delivers the last Phase 6 exit criterion (§71 item 1) — formatter, goto-
      definition, find-references, rename, publishDiagnostics for `.vs` — as a new `viso-lsp` crate
      with one clean leaf edge `("viso-lsp", &["viso-dsl"])` (17 crates). Two layers: a **pure
      analysis engine** (`index.rs`/`source_map.rs`/`position.rs`/`engine.rs`) with zero protocol
      dependencies, every operation a plain function from source + position to spans/edits and fully
      headless-unit-tested; plus a **thin synchronous stdio JSON-RPC frontend** (`rpc/` self-contained
      JSON + `Content-Length` framing, `server.rs` dispatcher, `src/bin/viso-lsp.rs` read→handle→write
      loop) with **no async runtime** — tower-lsp + tokio was rejected (AGENTS 25 "adapters, not
      owner"; also blocks headless engine testing). Two net-new frontend pieces in `viso-dsl`: the
      resolver now emits `SymbolDecl { id, name_range }` at the single `SymbolId`-mint site (no second
      tree walk) so goto/rename can locate the definition, and `Resolution` gains `Hash`/`Ord` so the
      reverse `def → use` index keys off it (compiler proper still compares by equality only). The
      formatter is a CST-driven normalizing re-layout per the DSL style rules (§21.5.2: unified
      indent, `:` binding + `;` terminator, block-brace placement, folded blank lines, **comments
      preserved**), anchored by `format(format(x)) == format(x)` idempotence + golden tests. All
      cold-path tooling (§7.2: HashMap/String/Vec are the right tools). Proven headless: 36 `viso-lsp`
      tests (engine goto/references/rename + formatter idempotence/golden + transport round-trips) and
      114 `viso-dsl` tests pass; `check-deps`/clippy/fmt clean.
  - **Deferred from Slice R (recorded, not swallowed):**
    - **Viso CLI** (`viso` command-line tool) — Phase 9 Studio/Inspector/CLI. This slice ships only a
      narrow `.vs` formatter/language-server bin, not a full `viso` CLI.
    - **`viso migrate`** — Phase 10 source-level Makepad migration tooling.
    - **Extended LSP methods** — hover / completion / signature help / semantic tokens / code actions
      / folding. This slice ships the minimal usable set: goto-definition / find-references / rename /
      formatting / publishDiagnostics.
    - **Cross-package reference indexing** — `SourceMap` handles multiple open documents, but the
      reverse index is per-module; cross-package references wait for a workspace-wide index.

**Phase 6 exit criteria (doc §71) — ALL GREEN:**
- [x] formatter/LSP/goto/rename/reference usable — Slice R.
- [x] release needs no startup `.vs` parse — Slice P (AOT package).
- [x] hot reload is compile → validate → atomic patch — Slices K–O.
- [x] a failed compile keeps last-good UI — Slices K–O / Slice Q (shader last-good).
- [x] state/focus/scroll migration has explicit rules — Slices K–O.

**Phase 6 is complete.**

**Deferred past Phase 6 (doc §71+, renumbered — see doc-sync note below):** Phase 7 native widget
rewrites (Tier 1–6, doc §71); Phase 8 platform services / async / app framework (doc §72); Phase 9
CLI / Studio / Inspector / Web Delivery (doc §73 — the deferred state inspector rejoins here). The
old Phase 10 `viso migrate` migration tooling was **removed** from the doc (see doc-sync note).

---

## Doc-sync — 2026-09-05 文档更新对 Phase 1–6 代码的影响核对

> 用户 2026-09-05 用一份"去迁移化"的架构文档替换旧版(标题从「架构设计与重构迁移方案」→「架构
> 设计」),并把 CLI 拆到独立 `Viso_CLI.md`、把架构主体另存 `Viso_Architecture.md`(与
> `Viso_Architecture_and_Migration.md` 内容等同,仅表格 markdown 重排)。核对结论 + 由此产生的
> 代码改动清单如下。**核心结论:本次文档变化没有推翻任何 Phase 1–6 已完成代码的逻辑合同**——
> DAG / Identity / Ende / Node / Reactive / Layout / DSL 管线 / AOT / hot-reload 全部仍与代码一致。
> 需要跟进的只有「命名/编号/工具链设施」层面,以及一批**本就存在的 DEFERRED**(文档一直要求、
> 本次未新增,只是重新确认)。

### 文档本身发生了什么(不改代码,仅记录)

- **去 Makepad 迁移化**:删除 Part XXIV 迁移总策略、旧 §65–75 里的迁移叙述、Part XXVI API 迁移映射、
  Part XXVII Migration Tooling(`viso migrate` 整块)。改为 Part XXIV「Makepad 参考实现与设计经验」
  (§63,纯参考、不迁移、不建兼容层)。
- **Phase 重新编号 + 合并**(11 phase → 10 phase):Phase 0=§64 … Phase 9=§73。旧 Phase 9(Studio)+
  Phase 10(`viso migrate` 收尾)合并为新 **Phase 9 = CLI / Studio / Inspector / Web Delivery**;
  **`viso migrate` 被删除**(不再是路线图项)。已完成的 **Phase 6 现在是 §70**(旧 §71),Phase 7=§71。
- **CLI 大幅扩写**并独立成 `Viso_CLI.md`(§54.1–54.6):完整命令组、target 模型、`--json` 协议。属 Phase 9。
- 新增 §63 参考边界、Part XXVIII ADR 摘要(ADR-016..019)、Part XXIX 风险、Part XXX Definition of Done(§87)。
- 编译管线图(§70 / §38.1)把 IR 列表写作 **`UI IR / Reactive IR / Shader IR / System IR`**
  (旧版是 `UI IR / Binding IR / Shader IR`):"Binding IR"→"Reactive IR"改名 + 新增 "System IR";
  `viso dump` 子命令(§54)相应列 `ui-ir | reactive-ir | shader-ir | system-ir`。

### A. 由本次文档变化【直接】引入、应跟进的代码改动(命名/编号/设施)

- [ ] **A1 — 源码内旧 `§`/section 编号漂移,3 处需改**(Phase 重编号导致):
      - `crates/render/src/lib.rs:126` 注释 `§67 exit criterion "test scene 可绘制"` → 新 **§66**(Phase 2)。
      - `crates/ui/benches/node_arena.rs:1` 注释 `§68 exit criterion` → 新 **§67**(Phase 3)。
      - `crates/lsp/src/lib.rs:3` 注释 `doc section 71` → 新 **§70**(Phase 6)。
      注意:遵守"新 Rust 源不用 `§`+数字"约束——改的同时把 `§NN` 写成「section NN」/「doc section NN」。
- [ ] **A2 — `check-deps` → `arch-check` + `architecture.toml`**(doc §10.2 / §64 Phase 0 第 13 条):
      文档现在明确要求 `cargo xtask arch-check` + 一份机器可读的 `architecture.toml` 作为边界合同真值,
      而代码是 `cargo xtask check-deps` 且边界硬编码在 `xtask/src/main.rs allowed_edges()`,无
      `architecture.toml`。跟进:抽 `allowed_edges()` 到 `architecture.toml`、加 `arch-check` 子命令
      (可与 `check-deps` 并存/别名过渡),并同步全仓注释与本 todo 的 `check-deps` 措辞。
      **决策(2026-09-05 用户拍板):DEFERRED**——不阻塞 Phase 7,排到后续工具链设施小节;现阶段保持
      `check-deps` + 硬编码 `allowed_edges()` 不变。
- [ ] **A3 — IR 命名对齐 "Reactive IR"**(doc §70 / §38.1 / §54 `viso dump reactive-ir`):
      `crates/dsl/src/ir/binding_ir.rs` 及相关注释叫 "Binding IR",文档统一为 "Reactive IR"。纯改名
      (模块 + 注释 + `viso dump` 未来子命令名),不改逻辑。语义等同,低风险。**决策点**:改名 vs 保留
      "Binding IR" 作内部名并只在 `viso dump` 表层用 `reactive-ir`。
- [ ] **A4 — 本 todo / ADR 里过时的 Phase/§ 引用清理**:todo.md 里 "Phase 10 / §72+" 已按新编号更新;
      `docs/adr/{0008,0011,0012,0018}` 等含旧 §7x 引用(docs 被 gitignore,可选、非阻塞)。

### B. 文档一直要求、代码尚未做的既存缺口(本次文档【未新增】,只是重新确认;不是本次变化产物)

> 这些在旧文档同样存在,且多数已在旧 Slice ADR 里记为 DEFERRED。列在此处是为完整回答"文档 vs 代码"
> 的差,但它们不是"因文档变化才要改"。是否现在做需单独排期。

- [ ] **B1(旧 H1)— `component!` / `view!("...vs")` proc-macro 与 `#[component]` attribute 未实现。**
      doc §38.1 / §70 语言规则要求三个 Rust 入口共享同一 schema/HIR/IR;DSL 前端三种 grammar
      (CompilationUnit / ViewFragment / ComponentDecl)+ AOT + hot-reload 均已就绪,**只缺 Rust 宏表层**。
      `crates/ui-macros/src/lib.rs:57` 仅 `ui!`;`crates/macros/src/lib.rs:8` 标 "Planned #[component]";
      facade `crates/viso/src/lib.rs:57` 仅 re-export `ui!`。**Slice N 已明确 DEFERRED**。严格看这是 Phase 6
      语言规则第 2 条唯一未满足项。**决策(2026-09-05 用户拍板):保持 DEFERRED,先进 Phase 7**——
      承认 Phase 6 有此已知缺口,不阻塞 Tier 1 widgets;宏表层补齐留待专门回填。
- [ ] **B2(旧 H2)— 运行时 Computed/Effect 环检测(doc §20.1)未实现。**
      §20.1(Runtime 章节,旧版即有)要求运行时 computed/effect 图有 version stamp / evaluation stack /
      cycle diagnostic / debug source mapping,开发模式发现环给完整链路而非 hang。`crates/ui/src/reactive.rs`
      有依赖图 + wake 但无 evaluation-stack / 环诊断。**注意**:DSL **编译期** `computed` 环检测已存在
      (`crates/dsl/src/hir/component.rs` 拓扑排序 + E2105 带完整路径),缺的是**运行时动态图**的环诊断。
- [ ] **B3(旧 M1)— dense typed runtime IDs 部分缺失**:`PropertyId` / `EventId` / `ComponentTypeId` /
      `ShaderId`(doc §10.4.6)未定义(现仅 `StyleId` / `TokenId`)。多服务 Phase 7 widgets/paint/shader,
      归属后续阶段。
- [ ] **B4(旧 M3)— dsl/ir 无 Shader IR / System IR**:doc §70 管线要求 DSL 产出 Shader IR / System IR;
      Shader IR 现由独立 `crates/shader/src/ir/` 承载(Slice Q),System IR 无对应物。`system` 声明在
      Slice M 已 DEFERRED。System IR 是否属 Phase 6 收尾 or Phase 8 待定。
- [ ] **B5(旧 M2/L1/L2/L3)— 结构字段/ABI 属性与文档字面不符(低优先)**:NodeSlot 无 `flags: NodeFlags`
      (§16,注释标 Phase 0 只做 allocate/free/id);`NodeId` 未标 `#[repr(C)]`(§10.4.7);`StateId` 代码是
      generational `{index,generation}` 而文档写 `#[repr(transparent)] u32`(§18)——代码更强(带 stale 检测),
      **建议反向修文档而非改代码**;`StateSlot` 字段模型与 §18 字面不同。均本次未改动、非逻辑缺陷。

---

## Phase 7 — 官方 Widgets(doc §71):Slice 1 = viso-ui paint 全原语底层

> 用户拍板「先铺底层再做 widget」。把 `viso-ui` 的 paint 从「只画矩形」扩到「全原语(文字/图片/矢量)」+
> 内容驱动的 intrinsic-size 度量,使 `Length::Fit` 能按内容尺寸测量。下一片再集中写 Tier 1 控件
> (View/Container、Label、Image、Icon)。**架构 DAG 保持 `ui → render`,`viso-ui` 不引 `viso-text`**:
> `viso-ui` 只**存储 + lowering**,文字由上层(持 `TextSystem` 的 render/facade 层)算好塞进 content 列。
> **用户拍板「存测量结果 + paint payload」**:content 列存已测固有尺寸 + paint 载荷。crate 数不变(17)。

### 已完成(本片核心,已验证:166 tests / check-deps 17 crates 零变化 / clippy / fmt 全绿)

- [x] **1.1 `crates/ui/src/content.rs`(新)**:`enum Content { Text/Image/Path }`,各变体载 render 原语数据 +
      `natural: Vec2` 固有尺寸;`Content::natural()` 供 measure 读。坐标为节点-local,paint 时平移到 world。
- [x] **1.2 NodeStore content 列**:`content_payload: Vec<Option<Box<Content>>>`(cold,仿 semantics/grid_tracks
      的 `Option<Box>` 惯例);`alloc` 两臂锁步 reset/push `None`;`clear` 清空;访问器 `content_payload(id)` /
      `set_content_payload(id, Content)`(live-guarded,mark `MEASURE|LAYOUT|PAINT`);`LayoutTree::content_natural`。
- [x] **1.3 layout measure Leaf 臂**:硬编码 `0.0` → 读 `content_natural(root)`,`Length::Fit` 叶子测到真实内容尺寸。
- [x] **1.4 paint_tree emit 全原语**:背景 Quad 之后按 content 变体追加 `GlyphRun`/`Image`/`Path`(坐标平移到 world)。

### 待做(本片剩余)

- [x] **1.5 facade 文字 content 生产接缝**(已验证:2 新单测 + 166 tests / check-deps 17 crates 零变化 / clippy / fmt 全绿):
      **采用 Option 1「viso-ui cold `TextRequest` 请求列」**(用户「你按判据定」授权):`viso-ui` 新增 cold 侧列
      `text_request: Vec<Option<Box<TextRequest>>>`(`TextRequest { text, font_size, color }`,mostly `None`,不进热遍历);
      `BuildCx::text_request(handle, req)` 授权入口;facade `AppDriver` 持 `TextShaper`(`crates/viso/src/text_content.rs`:
      内嵌 `DejaVuSans-subset.ttf` + `TextSystem` + 持久 R8 atlas 纹理),build 后 `shape_pending_text()` 排干请求列、
      逐个 `TextSystem::prepare` shape、`set_content_payload` 写回(其 `MEASURE|LAYOUT|PAINT` 失效)。**理由**:请求是
      描述该节点内容的单一真相源、keyed by node;响应式重建只需重设请求→重 shape→重设 content→失效。接缝跑在 measure/layout
      **之前**(on_launch build 后;Layout phase reconcile 后、relayout 前)。本片只做**静态文字**,不做响应式。
      **新增 follow-up**:`shape_pending_text` 现 `dpi=1.0` 硬编码,DPI 应从 surface 密度取(见下 DEFERRED「DPI plumbing」)。
- [x] **1.6a headless 集成 + golden**(已验证:blessed + 逐通道 TOL=2 比对通过 / fmt / clippy 全绿):新增
      `crates/viso/tests/content_scene.rs` —— 从 **UI 侧**驱动的 golden(render golden 是手搭 `test_scene`,这个走
      `NodeStore → measure/layout → paint_tree → renderer → raster`,证明 content-bearing 节点端到端正确):Row 容器
      (dark bg)holding 一个 `Fit` 文字叶(共享 `test_glyphs` 字形串 + R8 atlas)+ 一个 48×48 图片叶(共享
      `test_texture` 棋盘纹理),baseline `tests/golden/content_scene.bgra8`(160×96×4=61440B,**同 render golden
      `quad_scene` 惯例:`*.bgra*` gitignore、BLESS=1 本地重生,不入库**)。ASCII dump 核实文字双行 + 棋盘块均正确
      栅格,dark-bg 角像素 (26,26,31) 符合 0.1/0.1/0.12。
- [x] **1.6b allocation/steady-state**(已验证:稳态两帧 alloc 恒为 [12,12]、frame_stats/buffer/texture/bind_group
      count 不增 / fmt / clippy 全绿):新增 `crates/viso/tests/content_alloc.rs` —— render bench 是手搭 `test_scene`,
      这个从 **UI 侧**驱动(与 1.6a 同场景:dark Row + `Fit` 文字叶 + 48×48 图片叶),每帧 `paint_tree` lower 进复用
      primitive buffer → `upload` → `submit`,装本文件私有 `#[global_allocator]`(集成测试独立 binary,计数器不串)。断言:
      两次相同稳态帧 alloc 数**相等**(paint/encode scratch + content 列无按帧堆分配,§7.1/§47);GPU buffer/texture/
      bind_group count 帧间不增;draw_calls/instances > 0(非空帧,断言非平凡)。实测每帧 12 次 alloc = headless backend
      定长 per-command instance-byte copy,帧间恒定不增长。**Slice 1 五节全绿,退出门通过。**

### 本片 DEFERRED(记进 backlog,不吞)

- [ ] **响应式文字**:**不**加 `StateValue::Text`(会破其 `Copy` 标量核心,克隆连锁进 `ComputeCx::get`/`EvalFn`
      等所有热路径,违「资源最省」)。改走**重建路径**:文字内容绑定变 → 目标节点重建 content 载荷 +
      MEASURE/LAYOUT/PAINT/SEMANTICS 失效。拆独立后续小节。
- [ ] **图片解码 / 图片 atlas**:全工作区无解码路径;本片 Image content 只接**现成 `TextureId`**。png/jpeg/svg
      栅格解码 + 图片 atlas 归属后续小节 or Tier 6 可选集成(doc §46)。
- [ ] **文字换行 / BiDi / 多字体**:`viso-text` 现为单 face、LTR、硬 `\n`;wrap/`max_lines`/overflow 留待文字子系统扩展。
- [ ] **DPI plumbing**:`AppDriver::shape_pending_text` 现 `dpi=1.0` 硬编码。应从 surface 的设备像素密度取真实
      `dpi_factor` 传给 `TextShaper::shape`(glyph 按该密度栅格化)。窗口 resize/移屏改密度时须重 shape 文字节点。
- [ ] **默认 UI 字体归属**:`crates/viso/fixtures/DejaVuSans-subset.ttf` 现由 facade 自持(`text_content.rs` include_bytes)。
      文字子系统成型后,默认 face + fallback 链应归 `viso-text` 拥有,facade 只选择而非内嵌资产。
- [ ] **B1 宏表层**(`component!`/`view!`/`#[component]`):Tier 1 先手写 `Component` struct,宏表层保持 DEFERRED。

### Slice 2 — Tier 1 widgets 框架 + View/Container(当前片)

- [x] 接缝补口:`crates/ui/src/lib.rs` re-export `viso_render::{Rgba,Rect,TextureId,PathCmd,Point,Stroke}`
      —— 已在 `viso-ui` 公开签名里(`Content`/`TextRequest.color`/`BoxStyle.fill`),让只依赖 viso-ui 的
      widget 能命名它们;零新 DAG 边(viso-render 已是 viso-ui 依赖)。check-deps 仍 17 crates 零变化。
- [x] `crates/widgets`:移除 lib.rs 里的 Makepad 关键字注释(违 no-Makepad-comment 约束);声明 `mod containers`
      + `pub use containers::{View, ViewStyle, view}`。
- [x] `crates/widgets/src/containers.rs`:`View`/`ViewStyle`/`view()`,`impl Component`(无 scroll→flex,有
      scroll→scroll),默认 `Role::Group`;子内容用单 `Box<dyn Fn(&mut BuildCx)>` builder 闭包(`Component::build`
      取 `&self` 故须 `Fn` 非 `FnOnce`,在 flex/scroll 调用点包一层);build/语义/layout 3 单测 + 1 doctest。
- [x] facade:prelude 加 `View`/`ViewStyle`/`view`;加 `pub mod widgets { pub use viso_widgets::*; }` escape hatch。
- [x] View 的 section 71 validation pack:build/语义/layout 单测在 `crates/widgets/src/containers.rs`(仅 viso-ui);
      golden/alloc/a11y/input-tape 集成放 `crates/viso/tests/view_widget.rs`(facade 侧,依赖已全)—— golden/alloc
      需 `viso::render`/`viso::gpu`,widgets dev-dep 反指 facade 会引循环,故取计划的退路落点。golden `*.bgra8`
      经 BLESS=1 生成、gitignore 不提交;alloc 稳态两帧相等(需 4 帧预热到 headless framebuffer 稳态,单帧不够)。
- [x] microbench 骨架:`crates/widgets/benches/view_build.rs`(criterion),measure build/layout/paint_tree 单帧;
      仅用 viso-ui(paint 输出元素类型由 `paint_tree` 推断,不必命名 `viso_render::Primitive`),零新 dev-dep 边。
      数字后续片补(本片先立骨架)。

### Slice 3 — Tier 1 widget:Label(已收官 2026-09-05)

- [x] `crates/widgets/src/lib.rs`:声明 `mod text` + `pub use text::{Label, LabelStyle, label}`(简单控件单文件)。
- [x] `crates/widgets/src/text.rs`:`Label`/`LabelStyle`/`label()`;链式 `.font_size`/`.color`/`.size`;
      `LabelStyle::default()` 两轴 `Fit`、font_size 14.0、color 近黑。`impl Component`——一个 `LeafStyle{ size,
      style: BoxStyle::NONE }` leaf + `text_request(TextRequest{text,font_size,color})` + `Semantics::role(Label)
      .with_label(text)`;shaping 由 facade `shape_pending_text` 那一步完成(不动 viso-ui/render/facade 底层)。
      `build(&self)` 取引用故 text `.clone()` 喂 request/label(冷路径 build-time,非热帧)。3 单测(单 leaf +
      text_request + Label 语义;setters 覆写 + 默认 Fit;非交互无 handler→Label 非 Button)+ 1 doctest。
- [x] facade prelude:`pub use viso_widgets::{Label, LabelStyle, View, ViewStyle, label, view}`;删注释里已兑现的 Label。
- [x] Label 的 section 71 validation pack:widget 单测在 `text.rs`(仅 viso-ui);golden+measure/a11y/alloc 集成放
      `crates/viso/tests/label_widget.rs`(facade 侧)。**Fit 测量硬约束**:Fit 叶子**作为 layout 根会填满 surface**,
      Fit-to-content 只在作为子节点时成立——故 golden 把 Label 包进一个 `view()` 容器,经 `store.arena().links(root)
      .first_child` 定位 label leaf,断言其 bounds 量到 glyph natural。shaping 走既有 test_glyphs fixture 旁路
      (`TextShaper` 是 `pub(crate)` 不可从集成测调),`set_content_payload(Content::Text{...})` 直接喂确定性字形。
      golden `label_widget.bgra8` 经 BLESS=1 生成、gitignore 不提交;alloc 4 帧预热 + 两帧相等 + `frame_stats`/
      `*_count()` 不变。
- [x] microbench 骨架:`crates/widgets/benches/label_build.rs`(criterion),measure `label(..).build`/layout/paint_tree
      单帧,仅 viso-ui(label leaf 承载**未 shape** 的 text_request,bench 计的是 widget 声明/布局/paint 成本,非
      shaping);Cargo.toml 加 `[[bench]] name = "label_build"`。烟测数字 build≈876ns / layout≈21.6ns /
      paint_tree≈9.2ns(短时长烟测,**非记录基线**;基线数字与 View 一起后续片补)。

后续片(不做,记此):
- Image(纹理 content,现成 `TextureId`)、Icon(Path content),各附同款 validation pack。
- **响应式 Label**:文字内容绑 state → 重建 text_request + MEASURE/LAYOUT/PAINT/SEMANTICS 失效;本片只静态文字
  (`StateValue::Text` 破 `Copy` 的连锁改动已在 Slice 1 被否,走重建路径)。
- 文字 **wrap / max_lines / overflow 截断 / BiDi / 多字体**:`viso-text` 现单 face/LTR/硬 `\n`,LabelStyle 暂不含 wrap 字段。
- 通过**公共 API** 真正驱动 facade `TextShaper` 的端到端 shape 集成(需 `Application` frame flow 或把 shape 接缝公开);
  本片 golden 沿用 test_glyphs 确定性惯例,不阻塞。
- **widget microbench 记录基线数字**(View + Label 一起补)。
- View 的 child-list / keyed children 抽象(现单 builder 闭包)。
- **B1 宏表层**(`component!`/`view!`/`#[component]`);Tier 1 先手写 `Component`。
