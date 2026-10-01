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
- [x] Spec codes `E1102`, `E1201`–`E1210`, `E1301` (`tests/lexer.rs`); every lexer
      code pinned to its variant, Appendix C row and a snippet (`tests/lex_codes.rs`).
- [x] `E1001`: unknown language version from `Viso.toml`/lockfile (§22,
      `frontend::check_language_version`).
- [x] `E1101`: identifier NFC normalization conflict; identifiers lex by XID and intern
      by NFC (`resolve/name.rs`, `resolve/resolver.rs`); the XID and NFC tables are
      Unicode 17.0 (ICU4X 2.3), pinned by a 17.0-only letter (`tests/lexer.rs`).

### D0.2 — Lossless CST and incremental layer

- [x] Green tree, red cursor, `root.text() == source` (`syntax/cst.rs`, `syntax/red.rs`).
- [x] Single-edit incremental re-lex (`syntax/reparse.rs`).
- [x] Incremental reparse: reuse unchanged green subtrees after an edit (§135).
  - [x] Test: an edit inside one member reuses every sibling member's green node.
  - [x] Bench (`benches/incremental_reparse.rs`, release): one keystroke in a
        1000-action component reparses in place in ~90 µs against ~8.1 ms for a full
        parse (10 actions: ~9 µs vs ~96 µs); the in-place cost still grows with the
        file through the token and unit tables.

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
  - [x] CLI glue: `viso check` (`tools/cli`) locates the project, maps its manifest
        to `load_package` and prints each diagnostic under its source line, with
        `Viso_CLI.md` §7 exit codes; `Viso.toml` spans carry byte ranges.
- [x] Per-owner member tables: a component's or system's members are visible only in
      it (two components may each declare `count`) and shadow module declarations; a
      handler's event resolves against the component its node instantiates, an `emit`
      against the enclosing one; a handler's payload pattern binds for its body
      (`resolve/scope.rs`, `resolve/resolver.rs`).

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
  - [x] `?.` requires an `Option` receiver: any other known type is `E2103` with a
        machine-applicable fix to `.` (member and method-call forms).
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
  - [x] Standard and widget event payloads: an implicit prelude (`resolve/prelude.vs`)
        exports the input types and payload records, shadowed by declarations and
        imports; each built-in widget lists its own events, and a handler naming an
        event its node does not take is `E3202` (`hir/widget.rs`, `hir/view.rs`).
  - [x] `task` signatures: a task call's arguments check against its parameters and
        its value is the declared result; `await` keeps its operand's type.
  - [x] Imports type as declared: records, enums, callable signatures, component
        inputs, events and payloads come from package-wide declarations
        (`hir/lower.rs` `Declarations` + per-module `ModuleScope`).
- [x] Event payloads `E3202`: a handler on a user component names a standard event or
      one the component declares; its payload pattern (irrefutable) types against the
      event's parameters; `emit` arguments match parameters by position or name and
      type against them (`hir/view.rs`, `hir/infer/body.rs`).
- [x] `E3104`: a property without a declared percent basis rejects `Percent`.
  - [x] Component inputs take the basis of the properties their component binds them
        to (forwarding settled to a fixed point across the package; an imported
        component's input keeps its basis).
  - [x] Percent components tracked through values (state/computed/const/locals,
        field access, indexing, record defaults) to a module fixed point; only
        length-holding properties are checked; calls and closures stay opaque.
- [x] `E3702`: `grid.*`/`stack.*`/`absolute.*` checked against the static direct parent
      (`Fragment`/`if`/`for`/`match` are transparent; view root, slot fill and component
      children are unknown parents); members typed from the container's child table.
- [x] Writability: `E2110` assignment to anything but a `state` or `mut` local (through
      field/index); `bind` source must be a State Lens `E3107`, of the property's exact
      type without `using` (`E2103`); `@bindable` pairs an input with an event of its
      type `E3701` (`hir/infer/lens.rs`, `hir/lower.rs`).

### D0.6 — Effects, capabilities, reads

- [x] Effect classes and call matrix `E2501`/`E2502` (`hir/effect.rs`).
- [x] Capability sets `E2601` (`hir/capability.rs`); the call graph spans the package.
- [x] Module-level `fn`/`action`/`task` bodies are effect- and signature-checked like
      component members.
- [x] State writes and `emit` are mutations (`E2502` in view/computed, `E2501` in
      fn/task/initializer); `on` handlers in a view check as event bodies; computed
      bodies, state initializers, input defaults, consts and field defaults are
      effect-checked.
- [x] Reactive read collection (`hir/reads.rs`); `E4201`–`E4203`.

### D0.7 — Source entries, diagnostics, formatter

- [x] One frontend behind `ui!`, `component!`, `view!("x.vs")` (`frontend.rs`,
      `ui-macros`); `E3002` view cardinality.
- [x] Shared `Diagnostic` type (`diag.rs`).
- [x] JSON diagnostic output matching §138: `viso check --json` streams §138 objects
      in the CLI event envelope — `schema_version`, byte + 1-based UTF-16 ranges (both
      ends), labeled `related`, `fixes` with applicability — for compiler, config and
      CLI diagnostics, then one `summary`; usage errors too (`tools/cli/src/output`).
  - [x] `expected`/`actual`: type mismatches (`E2103`, `E2102`: declared vs found,
        branches, range bounds, `?`/`?.` receivers, list indices, `bind` sides) and
        `E3001` fill them; `E3001` also carries a machine-applicable fix when a type
        follows `child`.
  - [x] Multi-file fixes: `Related` and `TextEdit` name their module; a candidate
        declared in another module points into its file; importing a non-exported
        name is `E2001` with an `export` fix in the target file, a missing one lists
        the target's nearest exports; the CLI resolves each span to its file (JSON
        `file`, human `:::` line).
- [x] Token-stream formatter (`crates/lsp/src/format`).
- [x] Formatter idempotence over every parser golden: `fmt(fmt(x)) == fmt(x)`, the
      parse leaves and codes unchanged, broken input included (`lsp/tests/format_golden.rs`
      over the shared corpus `dsl/tests/golden/parser_cases.rs`); generic `<` `>` hug.
- [x] LSP definition / references / rename / formatting / diagnostics (`crates/lsp`).

### Done

- [x] Every D0 `[ ]` above closed; Appendix C codes emitted by D0 each have a test
      (the frontend's unreachable non-root cast is an invariant, not a code).

---

## D1 — Behavior runtime (P1)

Goal: `fn`/`action` bodies execute. Today nothing runs a body; views only mount static
nodes.

### D1.1 — Behavior IR

- [x] Lower typed HIR bodies to a register-based Behavior IR: locals, calls, field and
      index access, record/enum construction, match decision trees, loops, `emit`.
- [x] Evaluation order per §133; operator lowering per §132.
- [x] Source map from every IR instruction to its origin (§134).
- [x] Record `..base` spread: checked and lowered (it was skipped by the checker).
- [x] Goldens under `crates/dsl/tests/golden/behavior/` (`BLESS=1`).

### D1.2 — Bytecode VM

- [x] Compact bytecode and an interpreter in a crate that `runtime` does not depend on.
- [x] Instruction, depth and memory budgets: `E7101`, `E7102` (§95).
- [x] Action transactions: writes commit at the action end, never mid-body (§86).
- [x] Bench: interpreter cost per simple action (release measurement).

### D1.3 — Typed native schema (§103)

- [x] Generated Rust schema: methods, `native action` vs query, thread domain,
      capability, ownership. `E6101`, `E6102`.
- [x] Native call op with a `native_calls` budget (§95.1 native call quota).
- [x] `viso schema <path>` query output (§139).

### Done

- [x] A counter component increments through a real `action` body in a headless test.

---

## D2 — UI runtime completion (P1)

Goal: every Core view construct reaches a live tree through all three lowering targets
(macro emit, hot-reload commit, AOT package).

### D2.1 — Handlers

- [x] Emit `UiHandler` in `ui-macros/src/emit.rs`, `hotreload/commit.rs`, `aot.rs`
      (payload typing `E3202` is checked in D0.5).

### D2.2 — Control-flow regions

- [x] `if`/`match` regions: mount, switch arms, `preserve` (`E3301`).
- [x] Keyed `for`: reorder moves retained nodes (`ir/keys.rs` already mints keys).
- [x] All three targets accept control flow; today they reject it.

### D2.3 — Components and slots in views

- [x] Resolve user components instead of defaulting unknown types to `NodeKind::Leaf`.
  - [x] Same-file component instances inline into the mounting view (`ir/mod.rs`),
        with per-instance function copies (`behavior/instance.rs`).
  - [x] Instance state as hidden mounted state `identity.state`, kept by hot reload.
  - [x] Input reads call the caller's argument entry; `emit` runs the caller's handlers.
  - [x] Inlined-view bindings substitute instance sources (`lower_view_bindings`).
  - [x] `E3711` for self-mounting, forwarded props on a multi-root view, and
        components of other files.
  - [x] Stateful instances inside `if`/`for`/`match`: per-mount kept states
        (`view/scope.rs`), initializers run with the region's scope values.
  - [x] Component instances in `ui!` fragments: Rust-scope `component!` types mount
        through their `build`; `E2001` on hot reload and the release package.
- [x] Widget schema registry: properties, events, slots, percent basis, from native
      declarations instead of the baseline table in `hir/widget.rs`.
- [x] Default slot `E3003`/`E3004`; slot cardinality `E3502`; unknown slot `E3501`.
  - [x] `slot` declarations in the component schema (`E2103` for a non-slot type).
  - [x] Cardinality counted over region arms; `SlotOutlet` placement checks.
  - [x] `fill` lowers into a native widget's children.
- [x] Two-way binding (§123) lowering: write-back through the `@bindable` event
      (compile-time `E3103`/`E3107`/`E3701` are checked in D0.5).
  - [x] Component input `bind`: argument reads the lens, the paired event writes it back.
  - [x] `bind … using` on a component input is `E3711`.
  - [x] Native widget `changed` delivery for a native `bind`.
    - [x] Built-in control responses (`viso-view` `Control`): toggle, slider, tab
          strip / radio group, text field; they read their value and range from
          handler-table entries and report `changed` / `selected_changed` /
          `submitted`.
    - [x] Controls carried by hot reload, the view package, region templates and
          the macros; a native `bind` writes back through the paired event.
    - [x] One dispatch per sample: a DSL handler runs on the target and bubble
          legs only; a pointer payload position is local to the node.
    - [x] `bind … using` on a native property is `E3711` (no converter yet).

### D2.4 — Reactive graph

- [x] Computed nodes with precise invalidation from `hir/reads.rs` edges.
  - [x] Reads propagate through calls: a computed's and a binding's read set
        include what the functions and computeds it calls read; a binding edge
        through a computed names the states beneath it.
  - [x] Component selection diagnostics are `E2005` / `E2006`, freeing
        `E4201` / `E4202` for their Appendix C meaning.
  - [x] Per-instance computed memo on the VM: a zero-argument computed is
        evaluated once and reused until a state or input it reads (transitively)
        changes; a faulted transaction discards the memo it touched.
- [x] Reactive property values reach their nodes (a text's content, a bound
      control's displayed value, a text field's seeded buffer).
  - [x] A `Text`'s or `Button`'s `text` and a `TextInput`'s `value` compile to
        handler-table value entries; `ViewBehavior` carries them with their
        target.
  - [x] The view runtime evaluates them at mount and re-evaluates only the
        entries whose read states changed, skipping an equal value.
  - [x] Controls project their value and range into semantic state.
  - [x] The macros, hot-reload commit, view package and region templates all
        deliver them.
- [x] Transaction batching; reactive cycle `E4202` at runtime.
  - [x] Several writes in one dispatch commit one revision and one delivery.
  - [x] Writes made while settling a frame re-flush up to a bound; past it the
        loop stops with `E4202`.

### D2.5 — Lengths at layout

- [x] `%` and `dp` fold to `LengthIr::Relative` / `Fixed` (`ir/length.rs`);
      `Length::Relative` in `viso-ui`, AOT tag `LEN_RELATIVE`.
- [x] `px`/`sp`/`em` terms: lower to a `LengthTerms` value resolved at layout with
      `scale_factor`, text scale and resolved font size (ADR 0033).
  - [x] `viso-ui` `LengthTerms` / `NodeLengths` bound in a sparse side table and
        folded at layout start against `LengthEnv`; only bindings whose inputs
        changed refold, and only a changed value dirties its node. Release
        bench `length_fold`.
  - [x] The facade follows the window scale factor.
  - [x] Lowering: a constant with any `px`/`sp`/`em` term lands in `LengthsIr`;
        the macros, the hot-reload commit (patching kept nodes in place) and
        the AOT package bind it, and packaged boxes match live ones.
- [x] Typography context passes resolved font size down the ancestry.
  - [x] `font_size` makes a node the typography source of its subtree; `em` and
        `%` inside it read the parent's, the root reads `base_font_size`.
  - [x] `resolved_font_size` reads a node's folded size.
- [x] `E3105` indeterminate basis and `E3106` non-finite length as debug warnings.
  - [x] The percent basis's definiteness threads through layout; an indefinite
        one resolves `%` to `0`.
  - [x] Each is reported once per node and kind through
        `take_length_warnings` in debug builds; `length_stats` always counts.

### Done

- [x] A todo-list app with `if`, keyed `for`, handlers and a user component runs
      identically through `ui!`, hot-reload commit and the AOT package.
  - [x] `view!`, the hot-reload commit and the package mount one `.vs` app and
        reach the same tree and boxes after every click (`dsl_todo_app.rs`).
  - [x] `ui!` mounts the same app declared by `component!`, the user component
        written where it is mounted (a `component!` declares one component and
        a `ui!` instance takes no property), and matches frame for frame.

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

- [ ] User traits, impls, general generics, const generics, `dyn` (`E2201`, `E2202`);
      `bind … using C` checks `C: TwoWayConverter<Source, Target>`.
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
