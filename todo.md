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

### Slice 4 — Tier 1 widget:Image(已收官 2026-09-05)

- [x] `crates/ui/src/component.rs`:新增 `BuildCx::image(handle, texture, uv, tint, natural) -> Handle` 声明接缝
      (仿 `text_request`,直接 `set_content_payload(handle.id, Content::Image{..})`)。Image 无 shaping 阶段——
      纹理已 resident,故 build 时直接写 content 最合理(零额外 facade 步骤);`set_content_payload` 已 mark
      MEASURE|LAYOUT|PAINT,widget 侧无需再失效。四元入参(`texture/uv/tint/natural`)不引 ImageSource/解码。
- [x] `crates/widgets/src/lib.rs`:声明 `pub mod image` + `pub use image::{Image, ImageStyle, image}`(简单控件单文件)。
- [x] `crates/widgets/src/image.rs`:`Image`/`ImageStyle`/`image()`;`image(texture, width, height)` 的
      `(width,height)` 是纹理固有像素尺寸,合一入参同时作默认 `size: Fixed(w,h)` 和 `Content::Image.natural`;
      链式 `.uv(Rect)`/`.tint(Rgba)`/`.size(Size)`。`ImageStyle` 无 `Default`(默认在 `image()` 内联:uv 全幅
      `FULL_UV={0,0,1,1}`、tint 白 `NO_TINT`、size `Fixed(natural)`)。`impl Component`——一个 `LeafStyle{ size,
      style: BoxStyle::NONE }` leaf + `cx.image(h, texture, uv, tint, natural)` + `Semantics::role(Role::Group)`
      (`Role::Image` 变体拆后续片)。`TextureId`/`Rect`/`Rgba` 是 `Copy`,`build(&self)` 无 `.clone()` 开销。
      3 单测(单 leaf + `Content::Image` 各字段 + Group 语义;setters 覆写 + 默认 size Fixed=natural;非交互无
      handler)+ 1 doctest(`TextureId(0)` dummy + `.tint(..)`)。
- [x] facade prelude:`pub use viso_widgets::{Image, ImageStyle, Label, LabelStyle, View, ViewStyle, image,
      label, view}`;删注释里已兑现的 Image。`viso::ui` 已 `pub use viso_ui::*` 故 `TextureId` 可达。
- [x] Image 的 section 71 validation pack:widget 单测在 `image.rs`(仅 viso-ui);golden+measure/a11y/alloc 集成放
      `crates/viso/tests/image_widget.rs`(facade 侧)。纹理走 **test_texture 棋盘格 fixture 惯例**——
      `gpu.create_texture(Bgra8Unorm) + write_texture(viso::render::test_texture())` 拿 resident `TextureId`,
      Image 的 `.build` 走**真实 `cx.image` 接缝**写 content(不绕),但纹理本身用 fixture 而非解码,像素确定。
      **Fit 测量硬约束同 Label**:Image leaf 包进 padded `view()` 容器(闭包捕获 `Copy` 值须 `move`),经
      `arena().links(root).first_child` 定位 image leaf,断言其 bounds 量到纹理 natural。golden `image_widget.bgra8`
      经 BLESS=1 生成、gitignore 不提交;alloc 4 帧预热 + 两帧相等 + `frame_stats`/`*_count()` 不变 +
      draw_calls>0 + instances>0。
- [x] microbench 骨架:`crates/widgets/benches/image_build.rs`(criterion),measure `image(..).build`/layout/
      paint_tree 单帧,仅 viso-ui(dummy `TextureId(0)`,不 raster;Image 无 shaping,bench 计的是 widget
      声明/布局/paint 成本);Cargo.toml 加 `[[bench]] name = "image_build" harness = false`。基线数字与
      View+Label 一起后续片补。

### Slice 5 — Tier 1 widget:Icon(已收官 2026-09-05)

- [x] `crates/ui/src/lib.rs`:re-export 行加 `LineJoin`(widget 要构造 `Stroke{ join: LineJoin }` 须能命名它;
      viso-render 已导出,零新 DAG 边;check-deps 仍 17 crates)。
- [x] `crates/ui/src/component.rs`:新增 `BuildCx::path(handle, cmds, fill, stroke, natural) -> Handle` 声明接缝
      (仿 Slice 4 的 `BuildCx::image`,直接 `set_content_payload(handle.id, Content::Path{..})`)。Icon 同为无
      shaping 呈现控件,载荷是几何而非纹理——build 时直接写 content 最优;`set_content_payload` 已 mark
      MEASURE|LAYOUT|PAINT,widget 侧无需再失效。
- [x] `crates/widgets/src/lib.rs`:声明 `pub mod icon` + `pub use icon::{Icon, IconStyle, icon}`(简单控件单文件)。
- [x] `crates/widgets/src/icon.rs`:`Icon`/`IconStyle`/`icon()`;`icon(cmds, width, height)` 的 `cmds:
      impl Into<Vec<PathCmd>>` 是矢量几何,`(width,height)` 合一入参同时作默认 `size: Fixed(w,h)` 和
      `Content::Path.natural`;链式 `.fill(Rgba)`/`.stroke(Stroke)`/`.size(Size)`。`IconStyle` 有 `Copy`+`PartialEq`
      derive;默认在 `icon()` 内联(fill=`Some(FOREGROUND)` 近黑不透明、stroke `None`、size Fixed)——图标默认
      「填充无描边」。`build` = `LeafStyle{ size, style: BoxStyle::NONE }` leaf + `cx.path(h, self.cmds.clone(),
      fill, stroke, natural)` + `Semantics::role(Role::Group)`(`Role::Icon` 变体拆后续片)。`build(&self)` 取引用
      故 `cmds` 用 `.clone()`(冷路径 build-time,非热帧;`fill`/`stroke`/`natural` 是 `Copy`)。3 单测(单 leaf +
      `Content::Path` 各字段 + Group 语义;setters 覆写 + 默认 size Fixed=natural;非交互无 pointer/key handler)
      + 1 doctest。
- [x] facade prelude:`pub use viso_widgets::{Icon, IconStyle, Image, ImageStyle, Label, LabelStyle, View,
      ViewStyle, icon, image, label, view}`;注释提及矢量图标控件。`widgets` escape hatch 自动带上。
- [x] Icon 的 section 71 validation pack:widget 单测在 `icon.rs`(仅 viso-ui);golden+measure/a11y/alloc 集成放
      `crates/viso/tests/icon_widget.rs`(facade 侧)。**与 Image 不同——不需 texture fixture**:cmds 用内联确定性
      几何(一个填充菱形),`Icon::build` 走**真实 `cx.path` 接缝**写 content,renderer 真栅格化(renderer.rs
      tessellate),golden 画出真实像素。Icon leaf 包进 padded 暗 `view()` 容器(闭包 `move`),经
      `arena().links(root).first_child` 定位 icon leaf,断言其 bounds 量到 natural。golden `icon_widget.bgra8`
      走 BLESS 机制(BLESS=1 生成、否则逐通道 TOL=2 比对),`*.bgra8` gitignore 不提交。3 集成测(golden+measure、
      Group 语义、稳态帧 4 预热+两帧相等+draw_calls>0+instances>0 零按帧堆分配)全绿。
- [x] microbench 骨架:`crates/widgets/benches/icon_build.rs`(criterion,照 image_build.rs),measure
      `icon(..).build`/layout/paint_tree 单帧,仅 viso-ui(内联三角几何,不 raster);Cargo.toml 加
      `[[bench]] name = "icon_build" harness = false`。基线数字与 View+Label+Image 一起后续片补。
- [x] 退出门全绿:`check-deps` 17 crates 零变化、`build --workspace`、`clippy --all-targets -D warnings`、
      `fmt --check`、`cargo test -p viso-ui -p viso-widgets -p viso`(viso-ui 166、viso-widgets 12 单测+4 doctest、
      viso icon_widget 3)全过。commit `eed516a`。

后续片(不做,记此):
- **`Role::Icon` / `Role::Image` 语义变体**:Icon/Image 现用 `Role::Group`;补专属 role(动 role enum + derive 臂 + 快照测)拆后续片。
- **ImageSource enum / 图片解码(png/jpeg/svg 栅格) / 图片 atlas**:全工作区无解码路径;Image 只接**现成 `TextureId`**。
  解码与 atlas 归属后续小节 or Tier 6 可选集成(architecture section 46 / DEFERRED)。
- **ObjectFit**(contain / cover / scale-down / none):Image 现只 Fill/stretch(paint 的 Image 臂把纹理拉满 world
  box);contain/cover 需 paint 侧算 uv 或 world 子矩形,拆后续小节。
- **响应式 Label**:文字内容绑 state → 重建 text_request + MEASURE/LAYOUT/PAINT/SEMANTICS 失效;本片只静态文字
  (`StateValue::Text` 破 `Copy` 的连锁改动已在 Slice 1 被否,走重建路径)。
- 文字 **wrap / max_lines / overflow 截断 / BiDi / 多字体**:`viso-text` 现单 face/LTR/硬 `\n`,LabelStyle 暂不含 wrap 字段。
- 通过**公共 API** 真正驱动 facade `TextShaper` 的端到端 shape 集成(需 `Application` frame flow 或把 shape 接缝公开);
  本片 golden 沿用 test_glyphs 确定性惯例,不阻塞。
- **widget microbench 记录基线数字**(View + Label + Image + Icon 一起补)。
- **图标字体 / SVG path 解析**:Icon 的 cmds 现由作者直接给出确定性 `Vec<PathCmd>`;icon-font(codepoint→glyph
  outline)与 SVG `d=` 属性解析归属后续小节。
- **ObjectFit 式 path 适配**(把几何缩放/居中进 leaf box):Icon 现按作者给的固有尺寸原样绘;fit 拆后续小节。
- View 的 child-list / keyed children 抽象(现单 builder 闭包)。
- **B1 宏表层**(`component!`/`view!`/`#[component]`);Tier 1 先手写 `Component`。

### Slice 6 — Tier 2 widget:Button(已收官 2026-09-05,第一个交互控件)

- [x] `crates/ui/src/component.rs`:`BuildCx` 加两个薄接缝(紧邻 `on_pointer`)——`on_key(handle, impl FnMut(&mut
      EventCx)+'static) -> Handle`(`set_key_handler`,焦点链 key 路由)、`focusable(handle, bool) -> Handle`
      (`set_focusable`)。与 `on_pointer` 完全同构,零新概念/依赖/DAG 边。
- [x] `crates/widgets/src/lib.rs`:`pub mod controls;` + `pub use controls::{Button, ButtonStyle, button}`;
      Tier 2 起交互控件进 `controls/` 目录(为后续 CheckBox/Toggle/Radio/Slider/TextInput 立目录),Tier 1 呈现
      控件仍单文件在 crate 根。`crates/widgets/src/controls/mod.rs`:`mod button; pub use button::{...}`。
- [x] `crates/widgets/src/controls/button.rs`:`Button`/`ButtonStyle`/`button()`——全交互控件 v1。
      `button(label: impl Into<String>) -> Button`;链式 `.on_click(impl FnMut(&mut EventCx)+'static)`/`.style`/
      `.size`/`.background`。`ButtonStyle{ size, background, pressed }`(全 `Copy`)。点击=键盘激活=同一 `on_click`:
      回调用 `Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx)>>>>`(`SharedClick`)在 build 里 clone 进指针 + key 两个
      闭包(build 时无法 clone `dyn FnMut`,故共享;运行时指针/键盘不并发,`RefCell` borrow 安全)。`build`:背景盒
      leaf + `cx.state(Bool(false))` pressed 态 + `cx.bind(pressed, root, PAINT)`(按下只触发该节点 PAINT,不重建);
      指针 handler Down(PRIMARY)置 pressed、Up(PRIMARY)复位并 fire、Cancel/Leave 复位;`cx.focusable(root, true)` +
      key handler Enter/Space 按下(非 repeat)fire;`Semantics::role(Role::Button).with_label(label)`;可见文字子用
      既有 `Label::build` 组合(零新文字路径)。无 `on_click` 时 build 不 panic、handler no-op。
- [x] facade prelude:`pub use viso_widgets::{Button, ButtonStyle, ..., button, ...}`;`widgets` escape hatch
      自动带上。
- [x] Button 的 section 71 validation pack:widget 单测在 `controls/button.rs`(仅 viso-ui);golden/a11y/alloc/
      **input-tape** 集成放 `crates/viso/tests/button_widget.rs`(facade 侧,`PointerRouter`/`KeyRouter` 可达)。
      6 集成测:golden+measure(文字子沿用 test_glyphs 确定性惯例绕 shaper;`button_widget.bgra8` 走 BLESS、
      TOL=2、gitignore 不提交)、指针 press→release fire 一次(press 单独不算)、非 PRIMARY 不触发、键盘 Enter+Space
      各 fire 一次(repeat/up/未聚焦不触发)、a11y role==Button label=="OK"、稳态帧(4 预热 + 两帧相等 +
      buffer/texture/bind_group/frame_stats 不变 + draw_calls>0 + instances>0 零按帧堆分配)。
- [x] microbench 骨架:`crates/widgets/benches/button_build.rs`(criterion,照 icon_build.rs;Button 走
      `BuildCx::with_reactive` 因 `cx.state`,并 `.on_click(|_|{})` 含 handler-boxing 成本),measure
      `button(..).build`/layout/paint_tree 单帧,仅 viso-ui;Cargo.toml 加 `[[bench]] name = "button_build"
      harness = false`。基线数字与 View+Label+Image+Icon 一起后续片补。
- [x] 退出门全绿:`check-deps` 17 crates 零变化、`build --workspace`、`clippy --all-targets -D warnings`、
      `fmt --check`、`cargo test -p viso-ui -p viso-widgets -p viso`(button_widget 6 集成测 + 全套单测)全过。

后续片(不做,记此):
- **hover / enter / leave 合成 + click-to-focus**([todo.md:91](todo.md#L91),[93-94](todo.md#L93-L94)):Button 的
  pressed 现只由 Down/Up/Cancel/Leave 驱动;hover 高亮态、指针进出合成、点击自动聚焦拆后续小节(需 input 层合成
  enter/leave 事件)。
- **disabled 态**:`Semantics` 现无 disabled/pressed/action 字段;加语义化 disabled(禁用不接事件 + 语义标记)
  拆后续片。
- **Tab / Shift-Tab 焦点遍历**:`KeyRouter` 注明遍历是 caller policy,未内建;焦点环遍历拆 input 子系统后续片。
- **回调 / action bus**:Button 现 `on_click(FnMut(&mut EventCx))` 直连;统一 action/message 总线(若需要)拆后续。
- **widget microbench 记录基线数字**(View + Label + Image + Icon + Button 一起补)。

TextInput 后续片(源码审计确认的缺口,不吞,留待专门的文本编辑片):
当前 TextInput 是单行编辑骨架——已通:键盘输入/退格删除、Left/Right/Home/End、Shift+方向键键盘选区、聚焦、
IME preedit/commit 上屏 + 光标在 preedit 内。以下均 DEFERRED(`text_input.rs:39-42` 模块 docstring 已列):
- **on_change 接编辑后文本**:`on_change` 现只存不调;buffer 在 handler 返回后才 re-shape,handler 拿不到编辑后
  文本(`SharedChange` 注释 `text_input.rs:71-76`)。需在 reconcile 产出 post-edit 文本后回调——优先补的一项。
- **剪贴板 cut/copy/paste**:全工作区无实现,`platform` 只有一句 doc 列了 clipboard 职责。需 `Cut/Copy/Paste`
  意图 + 平台剪贴板服务接线。
- **指针拖拽选区 + 双/三击选词**:pointer handler 现只在 Down 时 `request_focus`,无字符命中测试;需 shaped glyph
  几何做 hit-test(`text_input.rs:34-36`)。
- **grapheme 簇步进升级**:`prev/next_boundary` 现走 `is_char_boundary`(码点步进);rustybuzz 有 cluster 信息
  (`text/src/shape.rs`)但编辑引擎未用。升级为按簇步进(`text_edit.rs:36-43`)。
- **词级移动**:`Motion` 无 word 变体;key handler 只读 shift,不看 ctrl/alt。加 Ctrl/Alt+方向词跳。
- **placeholder / password 掩码**:无字段、无掩码 paint。
- **水平滚动裁剪**:无 per-field scroll offset/clip(长单行溢出不滚动)。
- **光标闪烁**:无 timer/blink 状态(依赖动画/定时子系统)。
- **完整状态 IME 同步**:preedit/commit + 光标在 preedit 内已通;缺向 OS 上报的 set-composing-region /
  cursor-rect(平台权威全状态,Android/iOS,`text_input.rs:41-42`)。
- **多行 / 换行 / wrap**:`Buffer` 明确单行(`text_edit.rs:150`),Enter 不插入;`Motion` 无 Up/Down。依赖
  `viso-text` wrap/多行(现单 face/LTR/硬 `\n`),与既有文本子系统 deferral 合并。

---

## Phase 7 — 官方 Widgets 进度总账(Tier 1–6,doc §71)

> 每个控件出一套完整 section-71 验证包(widget 单测 + golden 截图 + input tape + a11y 语义快照 +
> microbench + allocation profile),**每小节一提交**,做完接着下一个不停顿。golden `.bgra8` 一律 gitignore
> 永不提交(本地 `BLESS=1` 重新生成)。以下按 git 历史核实的实际状态,不是计划。

**Tier 1 — 呈现控件(单文件,crate 根):** View / Label / Image / Icon —— 全部 ✅ 收官(Slice 2–6 见上)。

**Tier 2 — 基础交互控件(`controls/` 目录):** Button ✅ / CheckBox ✅ / Toggle ✅ / Radio ✅ / Slider ✅ /
TextInput ✅(单行编辑骨架,后续片见上文 deferral 清单)。全部 ✅ 收官。

**Tier 3 — 布局 / 结构容器:** Scroll ✅ / VirtualList ✅ / Grid ✅(Slice H/I/J,见上「已完成」)/ Splitter ✅
(`splitter_build` bench)。全部 ✅ 收官。

**Tier 4 — 导航 / 浮层:**

- [x] **Tabs** ✅ —— 分段切换 + `Role`;facade 验证包 + microbench(`79ed737`)。
- [x] **NavigationStack** ✅ —— 页栈 push/pop + `Role::Navigation`(`50da50f` role / `e8f64c0` 控件 /
      `30513a0` 验证包 / `53b1fbf` bench)。
- [x] **Popup** ✅ —— overlay 顶层序 seam(`f3007e3` 顶层 paint 序 / `617e845` 控件 / `7551307` 验证包 /
      `29aa647` bench)。
- [x] **Modal** ✅ —— 全表面对话框 + focus-trap + restore-focus + `Role::Dialog`(`9c7c8ce` focus-scope /
      `96d4599` 控件 / `d77950a` 验证包 / `6e81a27` bench)。
- [x] **Sheet** ✅ —— 边缘滑入抽屉;**同时建成完整动画时钟子系统**(Sheet 是其第一个消费者)。七提交:
      `3b83a17` 无夹取 world-space translate 槽(viso-ui)/ `cc27995` 可注入 `FrameClock` + 每帧 delta
      (runtime seam)/ `ff81c09` `AnimationRegistry` + `Easing` 缓动(viso-ui)/ `2c223cc` facade 经 frame
      loop 消费动画时钟(`FlushStateTransactions` 臂 tick + `wants_animation` + TRANSFORM-gated
      `resolve_transforms` 缺口修正 + 动画活跃自续 beat + `EventCx::request_animation` 延迟 seam)/
      `71ef946` Sheet 控件 + 单测(`SheetEdge`/`SheetHandle`/滑入滑出 + `on_done` 完成回调隐藏回焦)/
      `2aa429f` facade 验证包 / `1b616c2` Sheet + animation-tick microbench。
      bench 基线:sheet/build 2.72µs、layout 289ns、paint_tree 102ns;animation_tick/1 13.8ns、/64 984ns、
      /1024 14.7µs(线性,无 per-anim 开销)。
- [x] **Toast** ✅ —— 自动消失通知浮层;**同时建成真 section-25 一次性 timer 协议**(Toast 是其第一个消费者,
      等待期 0 帧空转,非动画时钟捎带的 ~240 帧)。判据("资源最省 + 稳态帧停",
      [[viso-macos-pump-autoreleasepool]] 教训)选真 timer 而非 `TranslateAnim` 便车。七提交:
      `3966771` 一次性 timer store `TimerRegistry`(viso-ui:arm/arm_request/earliest/fire_due/cancel,
      one-shot、on_fire 只拿 `&mut NodeStore`)/ `b2796b0` `FrameDriver::next_timer_deadline` +
      `resolve_control_flow` idle 发 `WaitUntil(deadline)` + `RuntimeCx::frame_now`(runtime seam)/ `e1d2906`
      macOS `untilDate`=deadline + 超时合成 `Wakeup`(Windows/X11 结构对称,真机验证留 CI;headless 视
      `WaitUntil`==`Wait`,测试用 ManualClock 手动跨越)/ `2f9a99e` `AppDriver` 持 timers +
      `FlushStateTransactions` drain+arm+fire_due + `EventCx::request_timer` 延迟 seam(facade 消费)/
      `9fc0eae` Toast 控件
      + 单测(= Modal 去 scrim/focus-trap/restore-focus,content `Role::Status` polite live region、非
      focusable、贴边 + 生成号守卫的自动消隐;`on_dismiss` 只在手动 dismiss 触发)/ `ded688b` facade 验证包
      + **ADR 0019**(`WaitUntil` 阻塞到 deadline、driver 拥有 timer store 的调度语义,§68 触发)/
      `85f9c8d` toast + timer microbench。
      bench 基线:toast/build 1.30µs、layout 124ns、paint_tree 52ns;timer/arm 19ns、earliest/1 1.5ns、
      /1024 1.27µs、fire_due/1 5.5ns、/1024 1.78µs(均线性、无 per-timer 隐藏开销)。
      稳态帧不变量取"确定性 + GPU 资源复用 + frame_stats 不变"(整帧经 HeadlessRaster 会重编码像素缓冲,
      非零 alloc);零 alloc 只断言在隔离的 `fire_due` 步。
- [x] **Window** ✅ —— 完整多窗口。scope 已定(用户明确):per-window state 重构 **+** 公共 `window()`/`WindowHandle`
      可**会话中开/关** OS 窗口 **+** 平台 close seam(`PlatformApp::close_window`,三后端 + headless,
      `WindowHandle::close()` 可程序主动关活窗)。七提交,已全部完成:
      - [x] 提交 1 平台 close seam(`PlatformApp::close_window` + 三后端 + headless 入队 `WindowClosed` +
        `RuntimeCx::close_window` + 单测)。
      - [x] 提交 2 `FrameDriver::on_window_closed` hook + scheduler 递减前调用(单一拆卸路径,OS 关与程序关同路)+
        loop.rs 单测。
      - [x] 提交 3 facade per-window state 重构(纯机械,零行为变化):`AppDriver{app,cx,windows:Vec<WindowState>}`
        (小 N 线性 Vec 非 map,§45),run_phase 逐窗迭代,wants_animation/next_timer_deadline 跨窗 fold,
        on_window_closed 先 `effects.cancel_all()`(cleanup then drop)再 retain 拆卸;+ `InputSample::window()`
        accessor + `EffectStore::cancel_all()`。既有单窗测试全绿证明零行为变化。
      - [x] 提交 4 window-open/close 延迟 seam(viso+viso-ui):`EventCx::request_open_window`/`request_close_window`
        + store queue/take + router 三站 drain + facade `run_phase` FlushStateTransactions drain→`cx.create_window`
        +`WindowState::open`(抽 on_launch GPU 起+build 复用路径)/`cx.close_window`(仅关,经 `WindowClosed`→单一
        拆卸);`WindowConfig` mirror + `WindowOpenRequest`(§3.5 viso-ui 不依赖 platform);headless 两窗集成测
        (开→2 独立 store/root、关→1、全关→loop 退)+ `DrivenApp` window_count/store_at/root_at/window_id_at。
      - [x] 提交 5 公共 `window()`/`WindowBuilder`/`WindowHandle`/`WindowConfig`(facade 应用级句柄,非节点控件,
        prelude 一点)+ id 回填 slot(`EventCx::request_open_window_tracked` + `WindowIdSlot` +
        `WindowOpenRequest.id_slot`,facade `create_window` 后回填 `id.0`);`WindowConfig` 重导 viso_ui,
        `viso_platform::WindowConfig` 私有留 drain 翻译点;widget 单测 5(open 记 tracked+id 未定、回填后 id()=Some、
        open 后 close 记请求、未开先 close no-op、无 content 也能开)。全 gauntlet 绿(check-deps/build/clippy/fmt/test)。
      - [x] 提交 6 facade 验证包(`viso/tests/window_multi.rs` 8 测:双窗独立 golden、多窗 tape(window() seam 开第二窗
        →两独立树、per-window pointer 路由、per-window resize 几何、`WindowHandle::close` 拆卸、跨窗 idle fold 零 CPU)、
        per-window a11y 快照、alloc profile(`--test-threads=1` 稳态 `frame_allocs[0]==[1]`+GPU 计数/frame_stats 不变))
        + **ADR 0020**(§68 多重触发:frame phase 语义 fan-out / node ownership = per-window NodeId 空间 / 公共
        window()/WindowHandle 生命周期;`Vec<WindowState>` 小 N 线性非 map 附 §45 依据 + 平台 close seam 单一拆卸路径)。
        golden `.bgra8` gitignore 永不提交。
      - [x] 提交 7 microbench(`viso/benches/window_frame.rs`,criterion):window/open、window/close、
        window/fan_out/N(N∈{1,4,16},经 `__test_support::drive_scripted` 真 AppDriver 帧循环驱动,
        window 非节点控件无 BuildCx 可测)+ `crates/viso/Cargo.toml` `[[bench]] window_frame`。release 实测
        fan_out 每窗边际 ~1.55µs 恒定(N=1:2.8µs / N=4:7.5µs / N=16:26µs)—— 线性 fan-out,无隐藏 per-window 开销,
        坐实 `Vec<WindowState>` 线性选择(§45 / ADR 0020)。`083d8b8`。
    ✅ **Window 收官 → Tier 4 全部完成**(Tabs / NavigationStack / Popup / Modal / Sheet / Toast / Window)。

**Tier 5 — 编辑器 / 结构工具类控件(doc §71,`viso-widgets` 内节点控件):** 待做,Tier 4 收完下一步开排。
每个仍出完整 section-71 验证包(单测 + golden + input tape + a11y 快照 + microbench + alloc profile),每小节一提交,
todo 随做随标、与源码同 commit(不独立提)。开工第一个控件时先读 makepad 对应实现([[viso-read-makepad-first]])。

- [x] **Dock** —— 可停靠 / 可拖拽重排的面板容器(停靠区 + 拖出浮动 + 拖回吸附 + 分隔拖拽调宽)。
      吃 Tier 3 `Splitter`(分隔条)做区内分割;pointer capture/drag 是核心(复用 Slice H 的 capture holder,
      §13 pointer capture);状态 = 停靠布局树(哪块面板停哪、比例);a11y `Role` 待定(landmark/region)。
      落 `viso-widgets/src/controls/dock/`(子目录,§5 复杂子系统)。
- [x] **FileTree** —— 树形文件浏览器(展开/折叠节点、缩进层级、单选/多选、键盘导航)。
      大目录吃 Tier 3 `VirtualList` 虚拟化(§12.4,不为 100k 文件挂 100k 节点);展开/折叠 = 结构 reconcile;
      稳定 key(路径)保持展开态([[viso-diverge-from-makepad]] 若 makepad 无对应取 Viso 更优);
      a11y `Role::Tree`/`TreeItem` + `aria-expanded` 语义。落 `viso-widgets/src/controls/file_tree/`。
- [ ] **code-editor primitives** —— 代码编辑器基础件(**primitives 非成品编辑器**):gutter 行号、
      语法高亮 span(样式区间,非 tokenizer 本体)、光标/选区渲染、可选 minimap。吃 Tier 2 `TextInput`
      的编辑骨架 + `viso-text` 子系统(shaping/行布局缓存,§20 不把每字符当节点);多行编辑、grapheme/IME
      感知。体量最大,可能自成子目录甚至独立 crate(§3.3 触发条件,动手前评估)。落 `viso-widgets/src/controls/code_editor/`(暂定)。

**Tier 6 — 重型 / 富媒体控件(doc §71):** 待做。**关键约束(§33/§4/§5/§7.2/§3.7):重型富媒体不进默认
`viso-widgets` 依赖图** —— 原文点名 "PDF/browser/map/chart should not be added to the default widget crate
dependency graph",大功能属 `extras/` 或 `integrations/`,靠冷路径 trait object / adapter 接入(§3.7 所有权阶梯:
媒体 codec = 集成 proven 实现,Browser = 纯 adapter)。每个仍需 section-71 验证包(能 headless 的维度)。

- [ ] **Markdown** —— Markdown 渲染(解析 + 排版到 Node 树)。落点待定:轻则 `extras/viso-markdown`,
      重(表格/代码块/嵌图)倾向 `extras/`;解析用 proven crate(§3.7 adapter),渲染走 Viso Node/Layout/text。
- [ ] **PDF** —— PDF 查看。`extras/` 或 `integrations/`(§33 明确排除默认 widgets);codec/解析 adapter。
- [ ] **Browser** —— 内嵌浏览视图。`integrations/`(纯平台 adapter,§3.7);各平台原生 webview 桥接。
- [ ] **Charts** —— 图表。`extras/`(§33 明确排除默认 widgets);GPU 直绘走 Viso render/paint。
- [ ] **Map** —— 地图。`extras/` 或 `integrations/`;瓦片加载 adapter + GPU 绘制。
- [ ] **Video** —— 视频播放。`integrations/`;平台解码 adapter + GPU 纹理上屏。

> Tier 6 每个控件的 crate 落点(`extras/` vs `integrations/`)与 adapter 边界,在其开工时按 §3.3/§3.7 定夺并开 ADR
> (跨 crate 依赖方向变化 = §68 触发)。当前仅登记,不预先建 crate。

### Tier 4 后续片(记进 backlog,不吞)
- 动画时钟扩展:scale/opacity/color 动画(现只 translate);spring 物理曲线;动画序列/编排;
  `prefers-reduced-motion` 无障碍(§15,减弱动画时直接跳到位)。
- 独立 `UpdateCx` FramePhase 相(若动画/timer 消费者增多需专相,开 ADR)。
- Sheet 拖拽消隐(下拉超阈值 dismiss);嵌套 sheet / sheet 栈。
- §25 UI task 协议完整化:`cx.spawn(async)` + 任务身份/唤醒/取消/scoped 所有权(Toast timer 若走路 (2)
  会先落地 `WaitUntil` 唤醒这一半,余下 async executor adapter 拆后续 Phase 8)。

---

## Phase 8 — 主线增量地基(Deferred backlog B 类:改 viso-ui/render 核心,非插件)

> 用户 2026-09-07 指示:先做 deferred backlog(B 类 = 主线增量能力,触发本是某控件),再回 Tier 5(Dock)。
> 已用两组核实 agent 逐项读实际代码判定真实状态(backlog 是历史快照,后续 Tier 2/3/4 控件已消化部分)。
> **已完成、不做**:World/transform column + clip folding(已泛化全节点,hit-test 折 clip)、Pointer capture/drag
> (Splitter + Slider 在用)、Focus on pointer-down(TextInput 在用)—— 三项主线机制均已随控件落地,核实有据。
> 每项仍出 section-71 验证包(能 headless 的维度)+ 每小节一提交,todo 随做随标、与源码同 commit(不独立提)。
> 性能类断言遵 §7.3(先测再改,benchmark/alloc profile 为准)。

**8.1 — Richer roles / state(a11y 地基;含 text-node label 失效)** —— 语义树今天只有 `role + label`(唯一派生态
`focused`);Tier 2 的 CheckBox/Toggle/Radio/Slider 收官时把 checked/value/range 藏在各自 reactive cell,**从未进语义
树**(derive 无状态 store)。补齐后这四个控件的 a11y 快照才真实完整。落 `crates/ui/src/semantics.rs` + component.rs
derive 路径 + 四控件接线。

设计定稿(§3.5 红线):活状态 **不** 进冷静态 `Semantics`,而是节点侧列 `Option<SemanticState>`;控件 build 登记一条
与 `bind` 平行的投影 binding,在 flush 阶段(两 store live)读 cell 值 → `set_semantic_state`(标 SEMANTICS);derive
只持 `&self` 读该列,镜像 `focused` 单槽先例。5 提交拆分:
- [x] 提交 1(semantics.rs 数据模型):加 `SemanticState`(Copy struct:`checked`/`value`/`range`/`expanded`)+ builder
      (`checked`/`slider`/`with_expanded`);加 `Role::Slider`/`Role::Radio`;`SemanticsNode` 加 `state: Option<SemanticState>`
      (默认 None);`Semantics` **不** 加活状态字段(保持冷静态)。删过时 "no state store today / borrow CheckBox" 注释。
      单测:新 role 区分 + SemanticState 构造/默认。
- [x] 提交 2(节点侧列 + 投影 binding,component.rs + binding.rs):`semantic_state` 侧列 + getter/setter(live-guard +
      赋值 + mark_dirty SEMANTICS),随 alloc 对齐;投影 binding(flush 阶段读值 → set_semantic_state);`derive_into` 读列
      填 `SemanticsNode.state`。单测:set 标 SEMANTICS;flush 投影后 derive 带 state;binding 变更 → 语义树随之变。
- [x] 提交 3(四控件接线,viso-widgets):CheckBox/Toggle `checked`;Slider `value`+`range`(role→Slider);Radio 每 option
      `checked`(role→Radio)+ 容器 Group;build 时写初值 + 登记投影 binding;删过时注释;每控件 a11y 快照测试(input tape
      驱动 flush+derive:勾选/拖动/换选项前后)。microbench(derive + flush 投影成本,`crates/ui/benches/semantic_projection.rs`:
      project_wake ~90.7µs / derive_with_state ~4.2µs)+ alloc profile(`crates/ui/tests/semantic_projection_alloc.rs` 稳态
      零 alloc,实证 SemanticProjector 复用 DepCursor 后 wake 零分配 —— 修掉 project 里 per-eval `DepCursor::new()`)。
- [x] 提交 4(text-node label 失效,backlog #4,viso-ui):`set_content_payload` 仅当 `Content::Text` 时补 `| SEMANTICS`
      (§11 `text content -> MEASURE+LAYOUT+PAINT+SEMANTICS`);Image/Path 无内在可访问名,保持 MEASURE|LAYOUT|PAINT 不加。
      读代码确认:`Content::Text` 只存已 shape 的 glyphs 非源串,可访问名仍来自 authored `Semantics.label`(TextRequest 被
      take 后清列不留),故不做 name-from-glyphs 伪回退,只做诚实的 SEMANTICS 失效。单测:改文本内容 → 标 SEMANTICS(冒泡);
      换图片 → 不标 SEMANTICS。
- [x] 提交 5(ADR,§68 触发 reactive semantics):`docs/adr/0021-reactive-semantic-state-projection.md`。记录 SemanticState
      节点侧列 + bind_semantic_state 投影 binding(flush 阶段两 store live 读值 → set_semantic_state,`&self` derive 只读列,
      不跨层读 StateStore,镜像 focused 先例)、SEMANTICS 失效契约(含 text-content-as-name)、Role::Slider/Radio、稳态零 alloc
      (SemanticProjector 复用 cursor,实证)。

**8.2 — Per-subtree 增量语义(性能地基)** —— 今天任一 SEMANTICS 脏即从 root 全树重建 `SemanticsTree`(component.rs
derive_semantics),无上一棵缓存、无 per-subtree 增量。落 `crates/ui/src/component.rs` derive 路径 + semantics.rs。

- [x] 提交 1(microbench + 基线 + 决策门,`crates/ui/benches/semantic_projection.rs`):加 CONTAINERS×LEAVES 大树(6101 节点:
      root + 100 容器 × 60 带非平凡 String label 叶子)+ `derive_full_single_label_change` bench(改一叶 label 冒泡到 root →
      计时 `derive_semantics_dirty(root)`)+ 启动断言 pin 规模 + 改动 label 确实入树。**基线实测:~317µs**(6101 节点全量重建)。
- [x] **决策门 → Gate A(预期结局):STOP,不建增量机器。** 判据:(1) 帧循环**零 live 消费者** —— `crates/viso/src` 无任何
      `derive_semantics*` 调用,widgets/reactive/component 的全部调用均在 `#[cfg(test)]`(toggle/checkbox/slider/radio 的
      `derive_state` 测试 helper、reactive.rs/component.rs 单测),出货帧支付 0 次/帧;(2) 扁平表示(`children: Vec<usize>` 绝对
      索引)无法廉价拼接复用子树,增量唯一可回收的是 label String clone,而 id→index map + 定位重建本身仍 O(n) 走树 —— 收益上限
      仅省 clone、不省走树。按 §7.3/§37 及 ADR 0021 已记的 deferral(0021 Consequences:「deferred until a live consumer needs
      it」),**记录基线、不加复杂度、无 ADR 变更**。~~提交 2/3(SemanticsCache + 增量派生 + 对比 alloc)~~ 仅 Gate B 触发,未触发。

**8.3 — resolve_styles bound-node 缓存(性能地基)** —— 今天 `resolve_styles` 对整个 dirty 数组 `0..dirty.len()` 全扫找
STYLE 标记(component.rs)。落 `crates/ui/src/component.rs`。

- [x] microbench(§7.3 先测):`crates/ui/benches/style_resolve.rs` —— 大树少量 styled 节点(6101 节点,每 20 个叶 1 个带
      style token 绑定 = 305 绑定叶),theme 换值 → flush 标 STYLE → `resolve_styles` 全扫 `0..dirty.len()`。启动断言 pin:恰好
      重解析 305 个绑定叶、绑定集 < 树 1/4。**基线实测:swap+flush+resolve 全程 ~10.7µs**(内含 6101 次 bitflag `intersects`
      全扫 + 305 次 `StyleId::resolve` 真实工作)。
- [x] **决策门 → Gate A(跳过缓存):不加 bound-node 列表。** 判据:(1) `resolve_styles` 帧循环**零 live 消费者** ——
      `crates/viso/src/lib.rs::relayout_and_paint` 做 relayout/transform/repaint,**无 STYLE resolve 阶段**,`resolve_styles`
      仅被 component.rs 单测调用(STYLE 增量层已实现未接入帧循环);(2) **全扫不热** —— 整个 swap+resolve 才 10.7µs,6101 次
      bitflag `intersects` 全扫是其中极小一部分,缓存省掉的正是这部分、收益微乎其微,却要引入随 bind/unbind + 节点生死维护的列表
      (§8.2 冷数据/一致性负担)。按 §7.3 及 backlog 原文「cache when a huge tree makes the scan hot (measured, not now)」——
      **记录基线、跳过、无 ADR 变更**。~~bound-node 列表 + alloc profile~~ 仅扫描测热才做,未测热。

**8.4 — hover / enter / leave(输入地基,从零)** —— 今天只有窗口级 `PointerPhase::Leave`(离开窗口边界),无 per-node
enter/leave 合成、无 hover 追踪、无控件用 hover 反馈。落 `crates/ui/src/input.rs` router + component.rs hover 追踪状态。

- [x] router 加"上一帧 hover 节点"追踪(NodeStore 上的 `hovered: Option<NodeId>` 或 hover 链);pointer Move 时 hit-test
      新目标,与上一帧差分,合成 enter(进入新节点链)/ leave(离开旧节点链)派发给对应节点 handler。
- [x] `PointerPhase` 加 `Enter`(per-node,区别于现窗口级 `Leave`);或设计 hover 专用事件 —— 按最合理设计定(节点 enter/
      leave 与窗口 leave 语义不同,评估枚举 vs 独立)。DirtyClass:hover 状态变更默认 PAINT(hover 样式反馈)。
- [x] 第一个 hover 消费控件:给 Button 加 hover 样式反馈(hover 时背景变化),作为真实消费者验证合成正确。
      交互态盒子选择机制:ui `InteractionStyle` + warm `interaction` 列 + STYLE 门控 `resolve_interaction_styles`
      pass(免 theme,接进 `relayout_and_paint`);`resolve_styles`(theme token 折叠)因 WindowState 无 Theme 暂不接
      —— 记 ADR 0022 section 6/7。
- [x] 验证包(Button hover golden + a11y):golden 三态 —— `button_paints_three_distinct_quads_across_interaction_states`
      驱 resting/hover/pressed 三相位断言 root `Quad.color` 各异;a11y —— hover 非语义(ADR 0022 section 4)。
- [x] 验证包(收尾):input tape(`viso/tests/hover_tape.rs` 6 步 move-tape 经 `PointerRouter::route`:enter/leave
      按序合成、hover 节点追踪、嵌套链差分——含填充容器上移到 root 的间隙情形)+ microbench(`ui/benches/hover_diff.rs`:
      within-node ~83ns vs cross-node ~173ns,启动断言 pin 行为)+ alloc(`ui/tests/hover_diff_alloc.rs` 单线程:稳态
      within-node move `frame_allocs==[0,0]`)。§68(frame phase / 输入语义)→ ADR 0022 已记。

**8.5 — stop_propagation 收尾(输入,机制已通)** —— 核实:`Dispatched{ran,stop}` + `dispatch_chain` 三段 honor +
`EventCx::stop_propagation` 三链(pointer/key/ime)全通,但**无任何控件真正 consume 事件、无 dispatch 级 swallow 测试**,
且 `input.rs:137` 头注释仍写"Consume/stop_propagation is a later slice"(过时)。落 `crates/ui/src/input.rs` 注释 +
控件消费者 + 测试。

- [x] 让一个控件真正 consume:Modal scrim —— scrim pointer handler 无条件 `stop_propagation`(隔断背后),
      并按 `dismiss_on_scrim`(默认开)在 Down 上走 `set_open(false)` 关闭;强制对话框可 `dismiss_on_scrim(false)`。
- [x] 加 dispatch 级 swallow 集成测试:事件到某节点 consume 后,祖先/后续 handler 不触发(pointer + key 链各一,
      对照 `capture_target_bubble_order` / `route_key_reaches_focused_node_and_bubbles` 的 `[0,1,0]` → 消费后 `[0,1]`)。
- [x] 更新 `input.rs` 过时注释(机制已落地,非 later slice)。
- [x] 验证包:swallow 单测(pointer 链 + key 链各一)+ Modal scrim 单测(swallow+close / disabled 时 swallow-only)。
      无需 golden/bench(纯逻辑)。

**8.6 — VirtualList 稳定 key(key_of reorder)** —— 今天 logical_index 即 identity,数据 reorder 时行按位置重建而非按
身份保持(virtual_list.rs reconcile 按 `logical_index` 匹配 mounted)。落 `crates/ui/src/virtual_list.rs`。

- [x] `MountedItem` 加稳定 key(`ItemKey(u64)`,§29 ID 非 String);reconcile 按 key 匹配复用(同一 item 换 index 时
      保持挂载/状态/focus,只 re-anchor row_offset 不 rebuild)。`key_of == None` 逐字节退化为 index 匹配。
- [x] API:`virtual_list_keyed(style, item_count, key_of, item)`,`key_of: Fn(usize) -> ItemKey`(数据侧提供稳定 key)。
- [x] 验证包:单测(reorder → 同 key 行复用 bound==0、insert 只重建新 key、reorder 后锚点稳定、unkeyed 与 index 一致)+
      microbench(`reconcile_keyed_reorder_within_window` ~1.4µs vs crossing ~5µs)+ alloc(稳态 within-window reorder 零 alloc,
      `keyed_reorder_alloc.rs` `--test-threads=1`;顺带修 `scratch_old_mounted` swap 让 crossing 路径也零 alloc)。
      更新 ADR 0008(§7 hook 落地 + §68 identity 语义)。§12.4 虚拟化契约"stable item keys"—— 补齐这一条。

**8.7 — Grid advanced(体量最大;ADR 0009 列的 out-of-scope 六项)** —— 今天只 Fixed/Fr/Auto/Percent 四 track +
spanning 放置 + auto-flow;ADR 0009 明列六项高级能力全未做。落 `crates/ui/src/grid.rs` + 更新 ADR 0009(或开新 ADR)。
按子能力拆多小节,每节一提交:

- [x] `minmax()` / `repeat()` / `fit-content()` track sizing(`TrackSizing` 加 `Minmax`/`FitContent` 变体;`repeat` 为创作期展开辅助)。
      顺带删 grid.rs 三处陈旧 `#[allow(dead_code)]`(GridTracks/place_children/solve_tracks 已被 layout.rs 调用)+ 删 GridTracks 死字段 `auto_rows`。
- [x] named lines / template-areas(创作期命名放置,降解成数值 placement,运行期不碰 String §29)。`GridStyle` 加冷字段
      `column_line_names`/`row_line_names`(`LineNames = Vec<(Box<str>, u16)>`)+ `areas: Option<GridAreas>`(`GridAreas::from_rows`
      从 area 名网格解析每名的 bounding `CellRegion`,`.` 为空格)。facade `place_named` / `place_area` 在 grid 闭包内解析成
      `GridPlacement`(name 表 stash 在 BuildCx 上,冷/boxed,嵌套 grid save/restore);运行期 `place_children`/`GridPlacement` 零改动。
- [x] subgrid(子 grid 继承父轨道)。`GridStyle` 加 `subgrid_columns`/`subgrid_rows`(Copy 标量,默认 false),
      随 `LayoutInput::Grid` 下带,经 `subgrid_axes(index) -> (bool, bool)` hook 读。机制 = 专用递归入口:`layout_grid`
      参数化 `inherited_cols`/`inherited_rows: Option<(&[f32], f32)>`(父在子 cell span 上已解的轨道尺寸切片 + 父在该轴的 gap)。
      公有 `layout()` 分发器恒传 `(None, None)`;逐 child 循环仅对声明 subgrid 的 grid child 切父 `col_sizes`/`row_sizes` 递归调 `layout_grid`。
      subgrid 轴跳过模板构建 / auto-max / `solve_tracks`,原样采用父尺寸,用父 gap 算 prefix offset,以继承切片长度为权威列数,
      使内 cell 线与父线逐像素重合(含内部 gap);非 subgrid 轴照常自解(None 退化为旧路径,常见路径零开销)。
      验证:layout.rs bounds golden(列 subgrid 复现父 gapped 列线、span-at-offset 只取父 k..k+n 段、双轴 subgrid 内角落父线、
      混合轴继承列自解 Fr 行)。更新 ADR 0009(Subgrid 从 Known follow-up 移入 Landed follow-ups)。
- [x] baseline 对齐(跨 grid item 基线对齐)。`GridStyle` 加 `align_items: AlignItems { Stretch(默认)/Start/Center/End/Baseline }`
      (共用 `layout::AlignItems`,Copy 标量,带上 `LayoutInput::Grid`)。`layout_grid` 按 align_items 在 cell block 轴就位:
      Stretch 把 Fill child 撑到 cell 高(旧隐含行为)/ Start·Center·End 贴自身 measured 高的顶·中·底 / Baseline 使同 row 各 cell
      首行基线重合。基线来源:`Content::Text` 加 `baseline: f32`(空 run 为 0),经 `Content::baseline()` + `content_baseline` hook 暴露;
      Image/Path 返 `None` 退化顶对齐。row 共享基线取各 cell max,child block 偏移 = `shared − child_baseline`。
      验证:layout.rs 单测(混合高文本 cell 落一条基线;fixed child 在 Start/Center/End 偏移、Stretch 不动;Fill child 仅 Stretch 撑开),
      走新增 `alloc_leaf` + `set_content_payload` 测试路径。
- [x] spanning-item 对 Auto sizing 的贡献(`grid::distribute_spanning_auto`:span-1 定基线后,span>1 item 把
      `measured − Σtrack_prebase − 内部 gap` 的余量均分进它覆盖的 growable(Auto/Minmax/FitContent)轨道,max 进各轨道;
      Fixed/Percent/Fr 不吸收)。ADR 0009 Decision 4 refinement 落地。
- [x] per-node `GridScratch` hoisting(消除 `layout_grid` 每趟 ~12 个临时 `Vec`)。加 `GridScratch` 复用缓冲(12 buffer)+
      thread-local free-list 池 `with_grid_scratch`:每次借出即 clear-不-free、跑完归还;`layout_grid` 可重入(subgrid 递归时
      父仍持自身轨道切片),故用池而非单缓冲——嵌套调用借到独立 buffer,不会踩到祖先;池只增长到见过的最深 grid 嵌套。
      `prefix_offsets` → `prefix_offsets_into`(写入复用缓冲而非返回新 Vec)。UI 树/布局主线程独占(§26),thread-local 无锁开销。
      验证(§7.3 有数才宣称):alloc pack `grid_layout_alloc`(counting global allocator,`--test-threads=1`)—— 暖机后稳态
      12×20 grid relayout **零 alloc**;同机 A/B bench `grid_relayout_12x20` 前后:~11.20µs → ~10.44µs(criterion −6.4%,p<0.05,
      Performance improved)。ADR 0009 第六项从 Known follow-up 移入 Landed follow-ups。
- [x] Adaptive(doc §69 item 11 的另一半:响应式列数)。列数由容器宽度布局期解算:`GridStyle.adaptive_columns:
      Option<AdaptiveColumns>`(冷可选 Copy,挂 `LayoutInput::Grid`),`AdaptiveColumns{ mode: Fill|Fit, min, max: Px|Fr }`,
      构造 `auto_fill`/`auto_fit`。count 公式 `adaptive_column_count`:`floor((content_w+gap)/(min+gap))` clamp ≥1。
      `Fr` max 新增 `TrackSizing::FlexMin(min,fr)` 带下限 flex 轨道(pass1 min 进 consumed + fr 进 fr_total,pass2 只补增量
      share),故 `minmax(min,1fr)` 单趟精确拉满;`Px` max 复用 `Minmax`。auto-fit 在 solve 前据 placement 定尾部塌陷、只对
      存活列 solve(回收空列宽+gap,末列右边界=content 右边界),事后补零宽尾列保持索引;auto-fill 保留全部空列。边界(§55
      主导用例取舍):纯 Adaptive 列模板 / 行不做 / 不与 subgrid 列叠加。验证:grid.rs FlexMin solve 5 例+构造;layout.rs count
      公式+bounds golden(窄/宽/整除/gap 边界、minmax(min,1fr) 拉满、Px max 留白、auto-fit 塌陷、auto-fill 保留);alloc pack
      稳态零 alloc;bench `grid_relayout_adaptive` ~11.37µs 与纯 Fr baseline(~11.25µs)同噪声无可测开销。ADR 0009 第七项
      Landed follow-up。
- [ ] 每项验证包:布局单测(golden 布局 dump / bounds 断言)+ 复杂 grid golden 截图 + microbench(§36 layout 类目)。
      更新 ADR 0009 把对应项从 out-of-scope 移入 + §68 触发(layout sizing model 变化,ADR 必更)。

> **触发型、暂不强做(记此,待消费者到达再拉入切片):**
> - **measure-affecting token**:机制(per-edge DirtyClass)已具备,但当前唯一 token 消费者 `StyleId` 只有 paint-only 字段
>   (fill/radius),硬编码 STYLE|PAINT。需要一个真实 tokenized measure 字段(font-size/spacing/padding)的消费者才有
>   意义 —— 随第一个 text/spacing token 控件(Tier 5 code-editor 或主题化 spacing)落地。§68 触发(dirty 契约扩展)。

**Phase 8 收官后 → 回 Tier 5(Dock,已备两份调研:makepad Dock 设计 + Viso Splitter/capture/Tabs 基础)。**

---

### 字体子系统 —— 系统字体 + 回退链 + 复杂整形 + 彩色 emoji(当前片)

目标:Hello World 居中混排英/中/泰/彩色 emoji + 真·系统字体,把字体子系统未通处全部打通。
判据 = 性能/资源/效果/设计/易用;缓存原则参考 makepad。整形只在 facade 缝 `text_content.rs`(§3.5)。
彩色 emoji 复用现有 Image 管线(白 tint 透传预乘 RGBA),无新 GpuInstance/shader/ABI/真机 Metal。

**Phase A —— 系统字体 + 回退链 + 复杂整形(shader/ABI 全程不动)**

- [x] **A1 `text: fallback font store`** —— viso-text `FontStore` 持多 face 回退链(`chain: Vec<FontId>`,`chain[0]`
      为 primary;`load` 空链时 seed primary,单 face 行为不变);`push_fallback` 显式追加去重(load 不隐式扩链);
      `chain()`/`primary()`/`first_covering(c)` 查询。每 face 加 `glyph_count`(粗覆盖权重,仅排序回退候选)+
      `has_char(c)`(cmap 覆盖探针,shaping 仍是覆盖权威)。attempted-set/负缓存推到 A4(依赖 unicode-script,只在
      facade 缝消费,§57/§40)。验证:text_system.rs 4 单测(seed primary / cmap 覆盖 / first_covering 走链 /
      push_fallback 扩链去重)。
- [x] **A2 `text: itemized bidi + script shaping`** —— 整形改为链感知递归 itemizer:`shape(store, text)` LTR 快路径
      (`is_definitely_ltr` 廉价 RTL 扫描,纯 LTR 跳过 BiDi)+ `unicode_bidi::ParagraphBidiInfo`/`visual_runs` 混排分段;
      `shape_run` 按 cluster 分组,`.notdef` run 递归 reshape 到 `chain[1..]`(镜像 makepad,LTR/RTL 逻辑字节范围各自算);
      `ShapedGlyph` 加 `font: FontId`。**偏离计划字面「facade 缝」**:itemization+BiDi+回退递归是纯算法无平台依赖,
      落 viso-text(§3.5/§3.7),facade 从不整形。签名改动顺带穿 layout(`PositionedGlyph` 加 `font`,按 primary 取行度量)
      + `TextSystem::prepare`(逐 glyph 按 `g.font` 光栅,atlas key 已含 font 无改)。加 unicode-bidi/unicode-script 依赖。
      验证:5 新测(空链→空、无回退→.notdef、回退递归命中 tail face、LTR 快路径 cluster、单 face 全归 primary)+ 既有全绿。
- [x] **A3 `text: layout over itemized runs`** —— 排版消费分项 run:`layout` 每行先 `shape(store, line)` 拿到分项
      glyph(可跨 face),行垂直度量**每行独立**——从 primary face 的 ascent/descent/line-gap 播种,再对该行落到的每个
      face 取 max ascent / min(最深)descent / max line-gap 扩张(镜像 makepad layouter:高的回退 face 如 emoji/CJK 比
      primary 高,只留 primary ascent 会切顶)。基线推进 = 上行 descent 深度 + 两行 line-gap 取大 + 本行 ascent,故拉入
      高 face 的行把下一行相应下推。`PositionedGlyph.font` A2 已加,单 face→链整形 A2 已通;本节只补每行度量扩张。
      验证:2 新测(首基线=primary ascent、单 face 行距=face line height)+ 既有 multiline 步进/pen 推进全绿(16 单测)。
- [x] **A4 `text: system font provider`** —— viso-text `SystemFontProvider` trait + `SystemFallback` 负缓存
      (attempted-set 落此,`resolve_missing` 扫 `.notdef` → 按脚本/emoji 分组 → 查 provider → 扩链);facade
      `system_fonts.rs` CoreText 实现(objc2-core-text 族,`CFRetained` RAII 免手动 CFRelease):按角色+样本串查
      (`new_ui_font_for_language`+`for_string` 级联)、`glyph_count<=16` 拒 LastResort、sfnt 重组(SKIP_TAGS 跳
      sbix/CBDT/CBLC/COLR/CPAL,glyf 存在时丢 VAR_TAGS,tag-as-pointer 取表标签,checkSum/checkSumAdjustment 留零)。
      非 macOS 编译为返 `None` 的 stub(trait 缝已留)。provider→shaper 活线接入(遍历链+reshape)推到 A5「prepare
      over chain」。验证:viso-text 4 provider 测(覆盖不查、缺脚本查样本、emoji 单独查、负缓存不重问)全绿(20 测);
      viso 3 sfnt 测(LastResort 阈值、checksum 补零、重组目录 ttf-parser 可解析)全绿。
- [x] **A5 `text: prepare over chain + dpi`** —— glyph 准备遍历回退链;修 `crates/viso/src/lib.rs:552` dpi 硬编码。

**Phase B —— 彩色 emoji(复用 Image 管线)**

- [x] **B1 `text: color bitmap raster`** —— 加 zune-png;`ttf-parser::glyph_raster_image` 取 PNG(+CBDT premul BGRA)
      → RGBA 预乘,带 ppem/origin 放置元数据;不做 COLR/SVG。size-bucket 量化留给 B2 图集层(B1 只解码到原生尺寸)。
      落 `crates/text/src/color_raster.rs`(`ColorGlyph` + `rasterize_color_glyph`),单测覆盖 premul 数学 + RGBA/RGB PNG 往返 + 损坏 PNG 拒绝。
- [x] **B2 `text: rgba color atlas`** —— `GlyphKind{Sdf,Color}`(`bpp()` 1/4);`Atlas` 持 `kind`,像素缓冲按 `size²*bpp`,
      MaxRects packer 与 R8/RGBA8 共用一份实现(仅每行字节步长差 bpp)。`new_color()` 建 `Rgba8Unorm` 图集,`color_glyph()`
      走 `rasterize_color_glyph` 并把 strike origin 从 ppem 缩放到请求;`GlyphKey` 加 `kind`(dpx_q 复用作 size-bucket)。单测覆盖 4 bpp 分配 + RGBA blit 字节布局。
- [x] **B3 `text: per-glyph kind in prepare`** —— `FontFace` 加 `has_color_strikes()`(载入时判 CBDT/sbix,纯文本 face 零探测);
      `TextSystem` 持第二张 color atlas + `color_atlas_pixels/size/take_color_atlas_dirty`;`GlyphQuad` 加 `kind`。
      `prepare`:彩色 face 先探 `color_glyph` 命中出 `Color` quad,否则落 SDF `atlas.glyph` 出 `Sdf` quad。
      集测:outline face `has_color_strikes()==false`、纯文本 run 全 `Sdf` 且 color atlas 不脏。
- [x] **B4 `viso: two textures + color glyph run`** —— facade 建两张纹理(R8 SDF + RGBA color);`Content` 携彩色 glyph run。
- [x] **B5 `render: lower color glyphs to Image`** —— 彩色 glyph 降为 `Primitive::Image`(白 tint);headless golden。

**Phase C —— 字体来源重定向(取代旧 B6「内嵌 NotoColorEmoji」)** —— 不内嵌任何默认字体;默认读系统;用户可 init 自定义;网络字体由开发者自取字节后调库;wasm 默认不加载。

- [x] **C1 `viso: drop embedded default face, system-default UI`** —— 删 `text_content.rs` 的 `UI_FONT`
      (`include_bytes!(DejaVuSans-subset)`)与内嵌默认 face;`TextShaper` chain 起始为空,首 shape 经 provider
      用 `FontRole::Ui` 拉系统 UI face;无 provider(wasm)则空、不出字不 panic。同节落 CoreText 彩色 emoji 光栅缝:
      viso-text `trait ColorGlyphRasterizer` + `FontFace.is_color_emoji`/`postscript_name()` + `prepare` 加
      `Option<&dyn ColorGlyphRasterizer>` 形参 + `atlas.rs::color_glyph` 选择逻辑;facade objc2-core-text 实现
      `CoreTextColorRaster`(预乘 RGBA,BGRA→RGBA swizzle 不 un-premul);`TextShaper` 持一个并作 `Some(..)` 传入
      `prepare`。`load_font` 内部缝供 C2 公开 API + 测试注入。三个 `text_content.rs` 单测改经 `load_font` 注入
      `DejaVuSans-subset.ttf`(纯测试 fixture,非默认)。
      **修复彩色 emoji readback stride**:`CGBitmapContextCreate(..,bytes_per_row=0,..)` 让 CG 自选行对齐(补齐)stride,
      旧 `read_back_rgba` 按紧凑 `w*4` 连续读 → 首行之后全部错位(倾斜/透明)。改为查 `CGBitmapContextGetBytesPerRow`
      逐行走(照 makepad `color_emoji_render.rs`,保持预乘不 un-premul)。回归测试
      `color::tests::color_glyph_readback_has_opaque_pixels`——真机 CoreText 路径扫 emoji glyph,断言 readback 紧凑
      `w*h*4` 且有不透明像素(修前必挂)。
- [x] **C2 `viso: user font API (bytes / path)`** —— facade 公开 init 期 API 加载用户字体(字节 / 磁盘路径),入 chain 居前。
      `AppCx::load_font(bytes)` / `load_font_file(path)` 记录到 session-scoped 字节表,`AppDriver` 于 `A::new` 后 drain,
      每个 `WindowState::open`(启动 + 延迟 `window()`)把它们按序装入新 `TextShaper` 居首,先于系统回退。
- [x] **C3 `viso: WOFF2 是外部独立库,核心只吃 sfnt`** —— WOFF2 不写进核心:`libs/woff2`(vendor,workspace 成员
      `viso-woff2`,单 API `decompress(&[u8]) -> Option<Vec<u8>>` WOFF2→sfnt)是开发者自调的独立库,核心 `viso` 不依赖它。
      facade 完全移除旧 WOFF2 缝(删 `crates/viso/src/woff2.rs` 手写解码器、`pub mod woff2`、`to_sfnt` 自动探测、
      `brotli-decompressor` 依赖、`.woff2` fixture);`WindowState::open` 直接 `shaper.load_font(face, 0)`,`load_font`/
      `AppCx::load_font` 只吃 sfnt。开发者用法:`let sfnt = viso_woff2::decompress(&woff2_bytes)?; cx.load_font(sfnt);`。

- [x] **Hello World(末节)**:`examples/hello_world/src/main.rs` 改为居中 `label("Hello 世界 สวัสดี 🎉").font_size(48.)`,
      全走系统字体(无内嵌)。验证:macOS smoke `hello_world_shapes_all_scripts_from_system_fonts`——空 chain 经 provider
      拉系统面,英/中/泰出轮廓 SDF、emoji 出 RGBA color glyph,宽单行 `natural.x > 3·natural.y`;真机 CoreText 路径。

- [x] **flex 主轴居中(`Justify`)**:此前 flex 只有 cross 轴 `Align`,居中要靠外 Row + 内 Column 双嵌套凑,且窗口放大时子节点
      漂向右下角。补主轴分布 `Justify { Start(默认)/Center/End }`(`FlexStyle.justify`),arrange 在无 Fill child 吃掉余量
      (`weight_total == 0`)时按分数分配主轴空隙——语义取自 makepad Turtle 的 `Align{x,y}` 分数分布,落进 Viso retained
      arrange。Hello World 收敛为单个 fill 容器 `justify: Center` + `align: Center` 两轴居中一个 Fit 子节点。所有 `FlexStyle`
      字面量补 `justify: Justify::Start,`(行为不变)。验证:`justify_and_align_center_a_fit_child_on_both_axes` 单测——
      600×400 与放大到 1000×800 均居中(证不再右下角漂移);viso-ui + viso 全套 503 测试绿。

**ADR(git add -f)**:0024 已被 file-tree 占用 → 文本 ADR 顺延为 **0025 Text subsystem ownership**(external-backed 算法 + 缓存边界)与 **0026 System-font provider + color-glyph seam**(trait 在 viso-text、CoreText 实现在 facade;含彩色光栅缝)。彩色 emoji GPU ADR 不需。

---

**Phase D —— 文字层缓存 / 失效 / 换行地基(§37.11/§37.13 契约补齐,当前片)**

> 缘由:一次「Architecture 文字契约 vs 现有 viso-text 代码」符合性核对(2026-09-09)发现,Phase A–C 打通了 shaping/
> BiDi/fallback/system-provider/彩色 emoji(名实相符、有测试),但 §37.11(cache 失效规则)/§37.13(ownership 清单)
> 里**明确「必须拥有」、代码却根本不存在**的三处能力尚未落位:paragraph/shaping cache、按宽度 line-break、font
> revision/incremental invalidation + profiler counters。远端渐进 font provider(§37.13)同样缺失。当前 `TextSystem::prepare`
> 每次从零 `layout→shape`,§37.11 的"Text/宽度未变则不 reshape/re-linebreak"只靠 facade 节点 pending-request 粗门
> (crates/viso/src/lib.rs:583-599)间接达成,text 层自身无缓存边界。这些是 Tier 5 code-editor(§37.12 编辑数据结构)
> 的前置地基,故先于 Tier 5 补齐。
> 每小节仍出验证包(§7.3 先测再宣称:cache 命中/失效单测 + microbench + 稳态 alloc profile)+ 每小节一提交,
> todo 随做随标、与源码同 commit(不独立提)。动手前先读 makepad 对应缓存实现([[viso-read-makepad-first]]),
> 缺口/更优处按 Viso 方案走([[viso-diverge-from-makepad]])。

- [x] **D1 `text: width-aware line breaking`(纯 text 层能力,不碰 measure 管线)** —— §37.11「可用宽度未变则不重新
      line-break」的前置:今天 layout.rs:83 只 `text.split('\n')` 硬断行,`layout()` 连可用宽度参数都不收。补按可用宽度
      自动换行(word/grapheme/forced-overflow 三级回退,参照 makepad layouter 语义;复用 proven unicode 分词 primitive,
      §37.13 不自研标准算法);`layout(store, font, text, size, max_width: Option<f32>)` 收可选宽度约束,`None` 时退化为
      今天的硬断行(零行为变化)。`TextSystem::prepare` / facade `shape` 相应加 `max_width: Option<f32>` 直穿,调用点先
      传 `None`。**范围严格限定在 text crate + facade 直穿**,可完整 headless 验证(给定宽度→行数/行宽/断点断言),
      **不引入 measure 期约束下行**。这是 D2 缓存失效键(宽度)的语义前提,也是 DL1(measure 接线)的被消费能力,故排最前。
      - **DONE 2026-09-09**:D1a `linebreak.rs`(`word_break_offsets`/`grapheme_break_offsets`,复用 unicode-segmentation,
        标点粘连规则,6 单测);D1b `layout.rs` 全量重写——整行一次 shape 量出 per-cluster advance 前缀和(bidi-run-order
        无关),按前缀和选行字节边界,每接受行再对其字节区间 shape 一次做摆放(全段每字节 measure 一次 + placement 一次,
        绝不 per-candidate reshape;比 makepad 的 per-segment shape 且 wrapping 无 bidi 更优);word→grapheme→force-place
        三级回退保证终止;`LineMetrics` 按行 seed/expand 跨 fallback face 取 max ascent/min descent/max line-gap。D1c
        `prepare`/facade `shape` 加 `max_width_px: Option<f32>` 直穿,调用点(text_content.rs / render/lib.rs)传 `None`。
        验证包:`tests/text_system.rs` 集成测试(宽度→行数/行宽≤限/断点在词界/回退/每字形保全,共 34 通过)+ 6 linebreak
        单测 + `benches/wrap_line.rs`(criterion microbench:24 词段落 no_wrap≈15.6µs vs wrap≈98µs;`CountingAlloc` 断言
        wrap 分配 ≤ 8× 未换行基线,守 §20「measure 一次 + 每行一次」不退化为 per-candidate reshape)。fmt/clippy(--all-targets)
        /workspace test 全清。真正的 measure 期约束下行接线见 DL1。

- [x] **DL1 `layout: measure 期约束下行 + 文本 Leaf width-aware 求解 + resize reflow`(布局引擎地基,独立 ADR)** ——
      2026-09-09 measure 管线核对发现:measure 是纯 post-order、**无约束下行**(`fn measure(tree, root, scratch)` 无约束
      参数,§96 `LocalConstraints` 至今是 spec-only),文本一次 shape 定型、natural 固定;可用宽度只在其后 layout 期以
      `bounds` 出现(layout.rs:634),比"决定文本高度的时机"晚一拍;resize 只重跑 measure/layout **不 reshape**,故当前
      soft-wrap 无从触发。要让「约束宽 → 换行 → 高度」成立,须给 measure 引入约束下行(把 §96 `LocalConstraints` 从
      spec 落成真实 measure 输入)+ 文本 Leaf 的 width-aware 求解回调(类比 content_natural,facade 回接 D1 的
      `shape(max_width)`)+ on_geometry/缓存把宽度并入 reflow 触发维度。**这是布局引擎级改动,牵动每个 widget 的 measure
      契约,不属于文字子系统**,故从原 D1 拆出、开自己的 ADR(§96 自适应布局落地开端)。消费 D1 的宽度换行能力,排在
      D1 之后;是否早于 D2–D3 视 §96 落地节奏定(D2 缓存的宽度失效键在 DL1 接线后才有真实来源,但 D2 缓存本身可先以
      `max_width` 入键就位)。
      - **进行中 2026-09-09**:采用**两阶段 facade 驱动 reflow**(非给 measure 引入约束下行——measure 保持纯 post-order、
        对文本无知)。Phase A `shape_pending_text(None)` → measure → layout;layout 的 Flex 摆放循环里子节点宽度确定后调
        `LayoutTree::request_text_reflow(index, assigned_width)`(纯数据写,eligibility 全在 NodeStore impl:width 轴
        `Fill`/`Fixed` 非 `Fit` + `soft_wrap` + 宽度量化到整数物理 px 后与 `shaped_at_width` 差 >0.5px)。Phase B(lib.rs
        `FramePhase::Layout` 臂,`relayout_and_paint` 后、`absorb_measurements` 前)`reflow_wrapped_text`:drain 队列→按
        retained source 在 `Some(width)` reshape→`set_reflowed_content`(标 MEASURE|LAYOUT|PAINT **不标 SEMANTICS**,宽度
        reshape 不改可访问名)→relayout,循环上限 3(收敛靠 eligibility+单调性:reshape 只改高、只缩不增 natural 宽,Fit
        排除;cap 只是安全网)。收敛证明与 §20/ADR 0025 量化+epsilon 守卫见新 ADR。数据模型:`Content::Text` 加
        `shaped_at_width: Option<f32>` + `soft_wrap: bool`(shape 期从 TextRequest 拷到 payload,recorder 只读 content 列);
        `TextRequest` 加 `soft_wrap`。facade 为 wrap 运行保留 `wrap_sources: HashMap<NodeId, TextRequest>`(store 的 request
        列 drain 后 Phase B 靠它 reshape;仅 wrap 子集、freed 节点 reflow 时 `retain(is_live)` 清理)。widget:`LabelStyle`
        加 `soft_wrap` + `.wrap()` setter。resize 被 subsumed(`on_geometry` 标 root MEASURE|LAYOUT|PAINT→下帧宽度失配重触)。
        §61 counter:reflow pass 数经 `VISO_FRAME_TRACE` gate 打印首帧 double-shape。
      - **完成 2026-09-09**:实现 + headless 测试全绿。测试拆两处(§35/§66):recorder/队列/失效类/收敛在 viso-ui
        `component.rs` 单测 7 个(Fill 窄于 natural 入队其盒宽、non-wrapping 不入队、Fit 不入队、盒够宽不 reflow、
        `set_reflowed_content` 标 MEASURE|LAYOUT|PAINT 不标 SEMANTICS + 队列收敛 + 高度增长、亚像素抖动不重入 settled reflow、
        Fixed 宽 reflow 到定宽);shaper 宽度透传/换行在 `text_content.rs` 单测 2 个(真字体 fixture:soft_wrap 窄宽 reshape
        换行且记录 shaped_at_width、non-wrapping 忽略 assigned width 记 None)。facade `reflow_wrapped_text` drain 环靠构造 +
        ADR 佐证(集成测试全走 fixture bypass、无字体栈够不到 reshape 分支)。ADR `docs/adr/0027-width-aware-text-reflow.md`
        (Accepted)。已验证:`cargo test --workspace` 绿、`cargo fmt --all -- --check` 净、`cargo clippy --workspace
        --all-targets -- -D warnings` 净、`cargo xtask check-deps` OK(17 crates,边全在 §10 DAG 内)。**未做**:macOS
        example 眼验实际换行段落、`cargo bench -p viso-text` wrap 复核(引擎未改、仅传入宽度变,列为可选)。
- [x] **D2 `text: paragraph / shaping-run cache`** —— §37.11/§37.13 的 paragraph/shaping cache:viso-text 持 paragraph 级
      缓存(键 = text + font/feature/revision + 可用宽度),命中则跳过 reshape + re-linebreak;把「不 reshape」的边界从
      facade 粗门下沉到 text 层自身。依赖 D1(宽度是失效键之一)。
      —— 落地:`paragraph_cache.rs` 有界 LRU(cap 512,cap 0 停用)缓存 `layout()` 的 `Rc<Vec<PositionedGlyph>>`;键
      `ParagraphKey{font,text,font_size_bits,wrap_bits,revision}`,`wrap_bits` 用 NaN 哨兵折叠 `None`,DPI 不入键(位置为逻辑
      像素、密度无关,1x/2x 共享一条)。feature 轴留空(shaper 目前无 per-run features,doc 标 seam)。`FontStore` 带
      monotonic `revision`(load/push_fallback/mark_color_emoji bump),D2 一并带出、折进键作粗失效(D3 再做精准/scoped)。
      `TextSystem::prepare` 经 split-borrow 走缓存。验证:6 单元(命中/各轴独立 miss/None-vs-Some 不 alias/revision 强制重算/
      LRU 驱逐/cap0 停用)+ 3 facade 集成 + `benches/paragraph_cache.rs`(命中 ~17.9µs vs miss ~58.6µs ~3.3×,
      startup 分配不变量 hit*2<miss)。全 workspace test/clippy/fmt/check-deps 绿。纯 text 层内部缓存、无跨 crate/架构变更,
      不触发 ADR。
- [x] **D3 `text: font revision + incremental invalidation + profiler counters`** —— §37.13 ownership 清单里代码零命中的一条
      (grep `revision`/`counter`/`invalidat` 全 crate 无)。font/chain 变更 bump revision → 精准失效 D2 缓存(而非全清);
      §37.11「新增无关 coverage 不失效」在此兑现(revision 携失效范围);text profiler counters(reshape/re-linebreak/raster/
      atlas-upload 次数)接入 §36 profiler。与 D2 缓存耦合,D2 落地时一并带出 revision,counters 并入本节。
      落地取 coverage-scoped generation(Option B):缓存只存位置,三类 font 变更按能否改变缓存位置分类 —— mark_color_emoji
      (只翻 SDF↔color 路由,shaping 不读,零失效)/ load 注册进非空 chain(只登记 face,零失效)/ chain append(唯一能改位置,
      且只对 box 了 .notdef 的段)只此 bump coverage_generation。失效范围收敛到「本段是否 box 过 .notdef」一 bit:全覆盖段跨代恒命中,
      box 段下次 append 时重算(保守安全)。TextCounters(Cell<u64> reshapes/relinebreaks/rasters/atlas_upload_bytes)接入
      TextSystem → TextShaper → VISO_FRAME_TRACE 首帧 dump,帧边界 reset。验证:paragraph_cache 单元(unrelated_coverage_does_not_invalidate/
      boxed_paragraph_reshapes_on_new_generation 等)+ 3 facade(registering_an_unused_face_is_a_cache_hit/mark_color_emoji_does_not_invalidate/
      counters_track_reshape_raster_and_reset)+ bench 新增 assert_unused_face_keeps_the_hit(hit*2<miss)。全 workspace test/clippy/fmt/
      check-deps(17 crates)绿;bench hit ~16.2µs vs miss ~52.3µs ~3×;VISO_FRAME_TRACE 首帧 text-counter 行无 panic。纯 text 层内部
      缓存语义 + counter,不触发 ADR。
