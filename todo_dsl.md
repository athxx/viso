# Viso DSL — D0 → D1 → D2 → D3 → D4 → D5 → D6 → D7 → D8

Construction order follows `Viso_DSL_1.0.md` §151: D0 is P0 (Core frontend), D1–D6 are
P1 (Core runtime, hot reload, adaptive, game, shader), D7 is P2 (Standard), D8 is P3
(Advanced). A later phase never becomes a prerequisite of an earlier phase's vertical
slice. D9 (acceptance suites) grows alongside every phase. Work one verifiable sub-unit
at a time. A semantic change updates the spec, Appendix C and the tests in the same
change. Crate landing points follow the dependency rules: `viso-dsl` is the only
compiler, `viso-ui-macros` and `viso-lsp` reuse its frontend, `runtime` never depends
on `dsl`, and no second parser exists anywhere. A perf claim needs a release
measurement; otherwise it is labeled a hypothesis.

Gate for every section: `cargo xtask check-deps` · `cargo fmt --all -- --check` ·
`cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace` ·
`cargo xtask check-targets`.

---

## D0 — Core frontend (P0)

Goal: every Core construct parses, resolves and type-checks with spec error codes, and
the three source entries share one frontend.

### D0.1 — Lexer (§8–§19)

- [x] Streaming tokenizer, trivia preserved, errors carried per span (`syntax/lexer.rs`).
- [x] Longest-match unit suffixes, `1em` vs exponent, radix/separator rules.
- [x] Spec codes `E1102`, `E1201`–`E1210`, `E1301` (`tests/lexer.rs`).
- [x] `E1001`: unknown language version from `Viso.toml`/lockfile (§22,
      `frontend::check_language_version`).
- [x] `E1101`: identifier NFC normalization conflict; identifiers lex by XID and intern
      by NFC (`resolve/name.rs`, `resolve/resolver.rs`).

### D0.2 — Lossless CST and incremental layer

- [x] Green tree, red cursor, `root.text() == source` (`syntax/cst.rs`, `syntax/red.rs`).
- [x] Single-edit incremental re-lex (`syntax/reparse.rs`).
- [x] Incremental reparse: reuse unchanged green subtrees after an edit (§135).
  - [x] Test: an edit inside one member reuses every sibling member's green node.

### D0.3 — Grammar (Appendix A)

- [x] Declarations: import/export, component, system, record, enum, type alias, const,
      fn/action/task, input/state/computed/event/slot, attributes (`grammar/decl.rs`).
- [x] Pratt expressions, 15 levels (`grammar/expr.rs`); statements (`grammar/stmt.rs`);
      types (`grammar/types.rs`); view grammar (`grammar/view.rs`).
- [x] `E2801` record expression in control head; `E3001` removed `child`; `E3201`
      removed event arrow.
- [x] Full pattern grammar: record/tuple/binding subpatterns, `|` alternatives, ranges
      (`grammar/patterns.rs`).
- [x] `E2802`: non-associative operator chain (§63.1).
- [x] `E2004`: expression generic without turbofish / const argument without `const`.
- [x] Replace `Parse0001`–`Parse0005` with stable Appendix C codes.
  - [x] Add the parse-error rows to Appendix C first, then migrate `ParseErrorKind::code`.
  - [x] Golden test pins every parse-error code (`tests/parse_codes.rs`).
- [x] Parser golden: one positive and one negative case per EBNF production (§152,
      `tests/parser_golden.rs`).

### D0.4 — AST, module graph and resolution

- [x] Typed AST views over the green tree (`ast/`).
- [x] Name interner, durable 128-bit `SymbolId`, three namespaces, deterministic module
      graph (`resolve/`).
- [x] `E2001`–`E2003`.
- [x] `E2001` carries the nearest candidates in `related` and a `fixes` entry, ranked by
      edit distance and filtered by namespace (§105.2, §138; `resolve/suggest.rs`, types
      and module paths).
  - [x] Filter by expected type once member and variant lookup raise `E2001` (D0.5).
- [x] Filesystem package loader: source root, module path from file path, one module
      graph, per-file diagnostics (`package.rs`).
  - [ ] CLI glue: `tools/project` manifest → `load_package`, with the CLI command.

### D0.5 — Types and inference (§19, §73–§83)

- [x] Type lattice with `Dp`/`Px`/`Sp`/`Em`/`Percent`/`MixedLength`, widening rules
      (`hir/ty.rs`).
- [x] Numeric literals and conversions: `E2101`–`E2103`.
- [x] Unit literals by suffix, suffixed-integer range check.
- [x] §19.3 dimensional arithmetic: bare-number instantiation, `MixedLength`, `E2106`,
      `E2107`, `E2109` (`hir/infer.rs` tests).
- [x] State ordering `E2104`, computed cycle `E2105` (`hir/component.rs`).
- [x] Infer the remaining expression kinds: `RecordExpr`, `ClosureExpr` (`E2401`),
      `IndexExpr`, `OptionalFieldExpr`, `TryExpr`, `RangeExpr`.
- [x] Type statements and blocks: `let`, assignment target and value, `return` against
      the signature, loop `break` values.
- [x] Match exhaustiveness `E2301` and unreachable patterns `E2302` (`hir/infer/pattern.rs`).
  - [x] Integer-range and list-pattern exhaustiveness; `match` inside `view`.
  - [x] Pattern shapes typed against the scrutinee (`E2103`); refutable patterns in
        `let`/`for`/closure parameters `E2303`; misplaced `return`/`break`/`continue`
        `E2803`.
- [x] `E2108`: `format` template vs arguments (`hir/infer/format.rs`).
- [x] Property values typed against the widget schema: `E3101`–`E3103` (`hir/view.rs`,
      baseline schema in `hir/widget.rs`).
  - [x] Text slots take only `String`: `text: count;` is `E2103` with a
        machine-applicable `format("{}", count)` fix; `format` in a view reads its
        arguments as binding dependencies. The built-in `Display` set is fixed.
  - [ ] Schemas beyond the baseline (native declarations), imported record/enum types,
        `task` signatures, handler payload patterns.
- [x] `E3104`: a property without a declared percent basis rejects `Percent`.
  - [x] Component inputs take the basis of the properties their component binds them
        to (forwarding settled to a fixed point across the module).
- [x] `E3702`: `grid.*`/`stack.*`/`absolute.*` checked against the static direct parent
      (`Fragment`/`if`/`for`/`match` are transparent; view root, slot fill and component
      children are unknown parents); members typed from the container's child table.

### D0.6 — Effects, capabilities, reads

- [x] Effect classes and call matrix `E2501`/`E2502` (`hir/effect.rs`).
- [x] Capability sets `E2601` (`hir/capability.rs`).
- [x] Reactive read collection (`hir/reads.rs`); `E4201`–`E4203`.

### D0.7 — Source entries, diagnostics, formatter

- [x] One frontend behind `ui!`, `component!`, `view!("x.vs")` (`frontend.rs`,
      `ui-macros`); `E3002` view cardinality.
- [x] Shared `Diagnostic` type (`diag.rs`).
- [ ] JSON diagnostic output matching §138: `schema_version`, byte + UTF-16 ranges,
      `expected`/`actual`, `related`, `fixes` with applicability, atomic multi-file fix.
- [x] Token-stream formatter (`crates/lsp/src/format`).
- [ ] Formatter idempotence over every parser golden: `fmt(fmt(x)) == fmt(x)`.
- [x] LSP definition / references / rename / formatting / diagnostics (`crates/lsp`).

### Done

- [ ] Every D0 `[ ]` above closed; Appendix C codes emitted by D0 each have a test.

---

## D1 — Behavior runtime (P1)

Goal: `fn`/`action` bodies execute. Today nothing runs a body; views only mount static
nodes.

### D1.1 — Behavior IR

- [ ] Lower typed HIR bodies to a register-based Behavior IR: locals, calls, field and
      index access, record/enum construction, match decision trees, loops, `emit`.
- [ ] Evaluation order per §133; operator lowering per §132.
- [ ] Source map from every IR instruction to its origin (§134).

### D1.2 — Bytecode VM

- [ ] Compact bytecode and an interpreter in a crate that `runtime` does not depend on.
- [ ] Instruction and native-call budgets: `E7101`, `E7102` (§95).
- [ ] Action transactions: writes commit at the action end, never mid-body (§86).
- [ ] Bench: interpreter cost per simple action (release measurement).

### D1.3 — Typed native schema (§103)

- [ ] Generated Rust schema: methods, `native action` vs query, thread domain,
      capability, ownership. `E6101`, `E6102`.
- [ ] `viso schema <path>` query output (§139).

### Done

- [ ] A counter component increments through a real `action` body in a headless test.

---

## D2 — UI runtime completion (P1)

Goal: every Core view construct reaches a live tree through all three lowering targets
(macro emit, hot-reload commit, AOT package).

### D2.1 — Handlers

- [ ] Emit `UiHandler` in `ui-macros/src/emit.rs`, `hotreload/commit.rs`, `aot.rs`.
- [ ] Event payload typing: `E3202`.

### D2.2 — Control-flow regions

- [ ] `if`/`match` regions: mount, switch arms, `preserve` (`E3301`).
- [ ] Keyed `for`: reorder moves retained nodes (`ir/keys.rs` already mints keys).
- [ ] All three targets accept control flow; today they reject it.

### D2.3 — Components and slots in views

- [ ] Resolve user components instead of defaulting unknown types to `NodeKind::Leaf`.
- [ ] Widget schema registry: properties, events, slots, percent basis.
- [ ] Default slot `E3003`/`E3004`; slot cardinality `E3502`; unknown slot `E3501`.
- [ ] Two-way binding (§123): `E3103`, `@bindable` `E3701`.

### D2.4 — Reactive graph

- [ ] Computed nodes with precise invalidation from `hir/reads.rs` edges.
- [ ] Transaction batching; reactive cycle `E4202` at runtime.

### D2.5 — Lengths at layout

- [x] `%` and `dp` fold to `LengthIr::Relative` / `Fixed` (`ir/length.rs`);
      `Length::Relative` in `viso-ui`, AOT tag `LEN_RELATIVE`.
- [ ] `px`/`sp`/`em` terms: lower to a `LengthTerms` value resolved at layout with
      `scale_factor`, text scale and resolved font size (ADR 0033).
- [ ] Typography context passes resolved font size down the ancestry.
- [ ] `E3105` indeterminate basis and `E3106` non-finite length as debug warnings.

### Done

- [ ] A todo-list app with `if`, keyed `for`, handlers and a user component runs
      identically through `ui!`, hot-reload commit and the AOT package.

---

## D3 — Hot reload (P1)

Goal: a file edit reaches the running app as one transaction or not at all.

- [x] Pure stages `plan` → `diff` → `migrate` and atomic `commit` (`hotreload/`).
- [ ] Dev server: file watch → `compile_file` → plan → commit on the UI thread at a frame
      boundary; last-good kept on any failure (§94).
- [ ] Behavior bytecode swap together with the UI patch.
- [ ] State migration by `SymbolId`: keep, safe widening, record field with default,
      `@migrate(from:)` functions, reset notice (§94.1). `E5101`, `E5102`.
- [ ] Node migration: focus, selection, scroll, animation fields from widget schema
      (§94.2).
- [ ] Reload diagnostics shown in-app and through `--json`.
- [ ] Bench: edit-to-pixels latency for a one-property change (release measurement).

### Done

- [ ] Editing a label, a handler body and a state type in a running app each apply
      without losing unrelated state; a broken edit leaves the last-good UI.

---

## D4 — Adaptive UI (P1)

- [ ] Typed adaptive environment, `LocalConstraints`, `AdaptiveScope`, `SizeClass`
      (§96.2–§96.3).
- [ ] Layout-phase evaluation order and `E4204` adaptive cycle (§96.5).
- [ ] `SafeArea`, `KeyboardInset`, `DisplayFeature` (§96.6).
- [ ] State preservation across adaptive branches (§96.8).
- [ ] §96.10 acceptance scenarios as tests.

---

## D5 — Game Profile (P1)

Goal: Quick Game and system games with compile-time determinism, snapshots and tick
timers (§104–§111).

### D5.1 — Scheduler and clock

- [ ] `FixedUpdate` / `FrameUpdate` / `CollisionListener` hooks bound from `SystemIr`
      (§131), no hard-coded names.
- [ ] Fixed-step loop with `tick`, `fixed_dt`, `time_scale`, `paused`, `step(n)`;
      `frame.time() = tick × fixed_dt` (§106.1).
- [ ] `TickOverrun::{DropTime, SlowMotion}`; `game.overrun_ticks`, `game.dropped_time`.
- [ ] System order and `E9101`; tick budget `E9102`.

### D5.2 — Input

- [ ] Edge semantics: each edge seen exactly once across 0 or many ticks per frame
      (§106.2).
- [ ] `@derive(InputAction)` enums and `@const` `InputMap` with move axes, dead zone,
      diagonal normalization (§106.3).
- [ ] `E9107` missing gamepad/touch path for the target platforms.

### D5.3 — State tiers and determinism

- [ ] `@local` state; Simulation domain = hooks plus reachable callables (§106.4).
- [ ] `E9103` Simulation touching Local or Presentation return values.
- [ ] `E9104` non-deterministic sources in Simulation.
- [ ] `Snapshot` trait, auto-derived for value types; `E9105`.
- [ ] Presentation commands keyed `(tick, source_system, sequence)`, not redelivered on
      replay or rollback; debug draw stripped in release.

### D5.4 — Timers, snapshots, interpolation

- [ ] `Cooldown` / `TickTimer` value types; `Duration` → ticks at compile time (§106.6).
- [ ] Generated `GameSnapshot`, Ende encoding, `restore(snapshot(s))` equivalence
      (§106.7).
- [ ] `RenderFrame.alpha`, previous/current transforms, `teleport` (§106.9).

### D5.5 — Quick Game

- [ ] `QuickGame.start` / `fixed` lowering; startup transaction before the first tick
      (§105.1).
- [ ] QuickGame and the equivalent single system give the same result on one tape.

### D5.6 — World commands and queries

- [ ] Deterministic command buffer merge and conflict policy (§108).
- [ ] Stable query order, generational `EntityId`, `@derive(GameTag)` (§108.1).

### D5.7 — Game reload and tooling

- [ ] Reload-tier classifier from the Stable ID diff, reported in diagnostics (§110).
- [ ] Logic-only reload at a tick boundary; Presentation-only at a frame boundary;
      World Rebuild with stable entity keys.
- [ ] `@probe` trace, input tape (binary and text forms), snapshot hash, headless sheet;
      `viso test game`, `viso game record`, `viso game peek` (§110.5, CLI §22.3).

### D5.8 — Kit

- [ ] `viso::game::kit` schema: camera rigs, prefabs, behaviors, particles, synthesized
      sound; every method tagged Simulation or Presentation (§105.2).

### Done

- [ ] §155 game acceptance list passes headless.

---

## D6 — Shader Profile (P1)

- [ ] Shader declaration grammar replaces the `AdvancedItem` skip (§97).
- [ ] Shader types and syntax subset; `E8101`–`E8104` (§98–§99).
- [ ] Lower to `viso-shader` IR; instance ABI from `@shader_value` records (§101–§102).
- [ ] Shader reload: background compile, swap at frame boundary, keep the current
      pipeline on failure (§110.3).
- [ ] CPU reference vs one GPU backend golden within tolerance.

---

## D7 — Standard surface (P2)

- [ ] `effect`: run policies, dependency lists (§91).
- [ ] `task`: structured concurrency, `E4101`, `E4102`, `E4401`, `E4501` (§92).
- [ ] `resource`: load/key, policy, `E4301`, `E4302` (§93).
- [ ] `style` / `theme` grammar and lowering; `@styleable`, `@selector` `E3710`.
- [ ] Runtime capability denial `E6103` (§95).
- [ ] `@persist`: load before `start`, tick-boundary writes, suspend flush, migration;
      `E9106` (§106.8).
- [ ] `AudioProcess` real-time rules `E9108` (§108.3).
- [ ] Dev snapshot ring and rewind-and-replay after a logic reload (§110.4).
- [ ] Accessibility and localization checks `E3704`–`E3706`, `E3708`.

---

## D8 — Advanced surface (P3)

- [ ] User traits, impls, general generics, const generics, `dyn` (`E2201`, `E2202`).
- [ ] `template` / `part` (`E3601`).
- [ ] Handwritten `native` declarations.
- [ ] Multi-system game profile and physics integration contract.
- [ ] `cross_platform` float determinism: `viso::math` transcendentals, no FMA
      contraction, fixed reductions (§106.5).
- [ ] Release native lowering of System IR with bytecode differential tests (§108.2);
      speedup is a hypothesis until a release benchmark shows it.
- [ ] Replication and rollback netcode on the Simulation tier.
- [ ] AI structured edit; cross-backend validation.

---

## D9 — Acceptance suites (grows with every phase)

- [ ] §152 parser acceptance.
- [ ] §153 type acceptance.
- [ ] §154 runtime acceptance.
- [ ] §155 game acceptance.
- [ ] §156 human usability samples compile and format stably.
- [ ] §157 AI generation loop: diagnostics with fixes, tape-verified behavior.
- [ ] §158 Definition of Done.
