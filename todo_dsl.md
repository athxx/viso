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

### D2.6 — Animation (§U6)

- [x] `transition.background` / `transition.opacity` play (see D3's animation
      fields).
- [ ] The other animatable properties' transitions; each is E3711 until its
      property reaches its node: `translate` and `corner_radius` (the VM has no
      `MixedLength`), `scale` and `rotation` (no transform channel), text `color`
      (baked into shaping), and `width`/`height`.
- [ ] `NodeRef::animate` returning an `Animation` handle, with `animation_end`
      (§U6.3).

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
- [x] Dev server: file watch → `compile_file` → plan → commit on the UI thread at a frame
      boundary; last-good kept on any failure (§94).
  - [x] The commit is scoped to the view's own subtree: it frees and rebuilds
        in place under the same parent and sibling position, swaps only the
        view's static edges and region hooks, and keeps the rest of the window.
  - [x] `view!` records each mount (file, source, origin, root, state cells,
        host, static nodes) under the `hot-reload` feature; without it the
        record is a no-op and no source text reaches the binary.
  - [x] A dev-only watcher thread polls the mounted files, settles a burst of
        writes, skips content-identical saves by hash and wakes the loop.
  - [x] The window drains changes at the frame boundary and runs the
        transaction; each view keeps a revision and its last-good candidate,
        and a failed candidate changes nothing.
- [x] Behavior bytecode swap together with the UI patch.
  - [x] The host's VM states migrate by `SymbolId` through the same migration
        plan as the UI cells, not by name.
  - [x] A handler-body edit reloads in a running window with every state kept.
- [x] State migration by `SymbolId`: keep, safe widening, record field with default,
      `@migrate(from:)` functions, reset notice (§94.1). `E5101`, `E5102`.
  - [x] The candidate carries each state's type; the compatibility matrix
        decides keep, safe widening, record extension with defaults, an enum
        whose active variant survives, or reset.
  - [x] A reset is a `E5101` notice naming the state and both types; a
        `SymbolId` minted twice in a candidate is `E5102` and rejects it.
  - [x] `@migrate(from: "T")` is a registered `fn` attribute, checked
        (one parameter of the old type, returning the state's type) and
        lowered; the commit calls it with the old value instead of resetting.
- [x] Node migration: focus, selection, scroll, animation fields from widget schema
      (§94.2).
  - [x] Widget schemas mark their migratable fields (`MigratableState`: focus
        and animation on every layout widget, scroll on `Scroll`, selection on
        `TextInput`)
        and the UI IR node carries the mark.
  - [x] The structural diff aligns each parent's children by type, node name
        and kind, so an inserted or removed sibling leaves the rest kept, and a
        region aligns only with one of the same form and arm count.
  - [x] A node kept with the same type carries its focus, scroll offset and
        edit buffer across a structural rebuild by `NodeKey`; a lost focus or
        scroll offset is reported.
  - [x] Animation fields carry: every transition the runtime plays.
    - [x] A node's look reaches it: `background` and `opacity` compile to value entries
          on any native node (`ControlKind::Plain` when it drives nothing else) and are
          delivered by the macros, hot-reload commit, view package and regions;
          `NodeStore::set_fill` / `set_opacity`, opacity painting its subtree as a layer.
    - [x] `transition.background` / `transition.opacity` (§U6.1, §U6.4): the shown value
          interpolates after mount, retargets, and is instant under reduced motion.
      - [x] Prelude `Transition` / `Easing` / `ReducedMotion`; prelude record field
            defaults are lowered, so a `Transition { … }` literal fills what it omits.
      - [x] `NodeStore::transition` / `tick_transitions`: a flat list of moving
            fills and opacities, retargeting from the shown value; a fill mixes in
            premultiplied linear light; a direct write ends a move.
      - [x] Value delivery moves a changed look over its transition entry (read
            only on change, no dependency); the first value and reduced motion
            with `instant` show at once; the facade ticks and keeps beating while
            a move is in flight, then halts.
      - [x] E3703 for a non-`Transition` value or a non-animatable member; an
            animatable member the runtime does not play yet is E3711, not a
            silent no-op.
      - [x] `background` colors convert from sRGB to linear.
    - [x] An in-flight transition carries to the node that rebuilds its kept node.
      - [x] `MigratableState::ANIMATION` on every layout widget; the rebuild lifts a
            kept node's moves and resumes them once the new view delivers its
            values, a region's nodes included: the same target keeps its clock, a
            new one turns from the shown value.
      - [x] A structure-preserving edit's re-delivery does not cut a move
            (`NodeStore::present`).
    - The other properties' transitions are D2.6.
  - [x] Nodes a region mounts carry their state.
    - [x] The view runtime lists the nodes its regions show, each with its arm item
          and the keys of the `for` items around it.
    - [x] The commit lifts their state before the rebuild and settles it on the
          nodes the remounted regions build from the same kept item for the same
          keys; what no node takes counts as lost.
- [x] Reload diagnostics shown in-app and through `--json`.
  - [x] Each reload yields a stage-labelled event (revision, outcome,
        elapsed, counts, diagnostics with spans).
  - [x] A failed reload shows an in-app overlay over the last-good UI until
        the next good one.
  - [x] `viso run --json` streams the events in the CLI envelope.
- [x] Bench: edit-to-pixels latency for a one-property change (release measurement).
  - [x] Release measurement split into watcher detection and reload +
        relayout + repaint (median 44 ms, of which the pipeline is 0.9 ms).
- [x] Platform file events instead of polling, removing the poll and settle
      latency that dominates edit-to-pixels.
  - [x] kqueue (macOS, iOS, FreeBSD), inotify (Linux, Android) and
        `ReadDirectoryChangesW` (Windows) over each file's directory, so atomic saves
        report; a file no backend covers is polled as before.
  - [x] A 5 ms quiet window where every write reports (kqueue, Windows), none where
        an event marks a completed write (inotify).
  - [x] Release edit-to-pixels on macOS: median 44 ms → 6.6 ms (detect 5.8 ms);
        Linux and Windows compile and lint clean but are not measured.

### Done

- [x] Editing a label, a handler body and a state type in a running app each apply
      without losing unrelated state; a broken edit leaves the last-good UI.

---

## D4 — Adaptive UI (P1)

Goal: views read a typed, layout-phase environment (`env`) whose changes invalidate
exactly the readers they reach, with structural branches that converge or report
`E4204` (§96).

### D4.1 — Typed environment in the compiler

- [x] Prelude environment types: `Rect`, `Insets`, `WindowMetrics`, `LocalConstraints`,
      `KeyboardInset`, `InputCapabilities`, `PointerPrecision`, `Locale`, `SizeClass`,
      `Orientation`, `LayoutDirection`, `DisplayFeature`, `Environment` (§96.2–§96.3,
      §96.7, §U10.1).
- [x] `env` resolves as the View execution domain's context binding typed `Environment`;
      a local shadows it; reading it outside a view is `E2111`; it is not writable.
- [x] `env.<field>` lowers to a per-component environment slot that each inlined child
      instance gets its own copy of; a bare `env` reads every field.
- [x] The view package carries each environment slot's field and anchor node, in both
      the live and the encoded form.

### D4.2 — Runtime environment

- [x] `AdaptiveEnv`: every field a revision cell raised only on change, with a settable
      `SizeClassPolicy`.
  - [x] Window-wide fields share one cell per field; `update_env` raises only the
        fields that changed.
  - [x] `SizeClassPolicy` (medium 600, expanded 840); a zero-width window is compact.
- [x] Anchored fields: `size_class` and `constraints` per instance, resolved from the
      nearest `AdaptiveScope` ancestor (else the window), written only on change.
  - [x] `anchor_env` / `anchor_cell` / `release_anchor` (generation-checked) and a
        wake cell per anchor.
  - [x] `settle_env` resolves incoming constraints through content-sized parents and
        scroll viewports; an unbounded scope inherits the class above.
- [x] The view host reads environment slots (VM values cached per revision) and region
      and value hooks depend on them; regional instances bind their own anchors.
  - [x] Static instances: the package's `env` reads link at mount, in live commit,
        release package and `__link_env` under the macros; released with the host.
  - [x] Region hooks depend on the window cells of every arm's env reads.
  - [x] An arm's instance env (`EnvTemplate`) merges into its locals: window cells
        shared, anchored ones on a per-mount anchor woken through the arm's pulse,
        released on unmount.
  - [x] Headless tests: reloaded and packaged view, `component!`, `view!` and `ui!`.
- [x] `AdaptiveScope` widget: classifies by its incoming max width (optional static
      `basis`); a resize that keeps the class rebuilds nothing.
  - [x] Schema widget (a column) with `basis: Option<MixedLength>`; a non-constant or
        non-`dp` basis is `E3711`.
  - [x] Built by `BuildCx::adaptive_scope` on every path (macros, live commit,
        release package, regions); hot reload re-marks a kept node; dead scopes are
        pruned at settle.

### D4.3 — Layout-phase evaluation and `E4204`

- [x] Frame order: layout → environment settle → state settle → relayout, bounded; a
      structure that does not converge reports `E4204` and keeps the last structure
      (§96.5).
  - [x] `settle_adaptive` (bounded by `ADAPTIVE_ROUNDS`): a frame that placed nothing
        and changed no environment input resolves nothing.
  - [x] `AdaptiveCycle` (`E4204`): the anchors keep their new values, the reactions
        are dropped, and the next frame is quiet.
  - [x] The facade's relayout runs it after every incremental layout.
  - [x] Tests: convergence across three layouts, a self-moving class as a cycle, and
        the frame loop resolving two scopes on its first frame.

### D4.4 — Safe area, keyboard and display features

- [x] The app feeds window metrics, safe area, keyboard inset, display features, input
      capabilities, text scale, reduced motion and locale into the environment;
      headless tests set them directly.
  - [x] Window logical size and scale factor, safe area and keyboard inset written at
        open (before the build) and on every geometry, safe-area and keyboard change.
  - [x] `reduced_motion` from the system appearance at open and on every change.
  - [x] Input capabilities: a touch default on phones and tablets, then inferred from
        the pointer kinds and hardware keys the window sees.
  - [x] A field is written only when its value changes; headless tests set the
        environment directly.
  - [x] Text scale from the system appearance, also moving the window's `sp` lengths:
        iOS content size category, Android font scale, Windows text size, the GNOME
        text-scaling factor, the Web default font size; macOS has none to read (1.0).
  - [x] Locale as BCP 47, with the layout direction its script reads in, at open and on
        every change: macOS/iOS preferred language, Android configuration, Windows user
        locale, Web `navigator.language`; Linux reads the process locale at launch.
  - [x] Display features per window at open and on every change: the macOS notch and
        Android display cutouts as `Cutout`, Web viewport segments as `Hinge`/`Fold`;
        iOS, Windows and Linux report none (no public source).
- [x] `SafeArea` and `KeyboardAvoiding` widgets pad from the environment, without
      double-padding under the root safe-area wrap.
  - [x] Native widgets lowered to a Column-like flex marked as avoiding, through the
        macros, hot reload (including restyle) and the release package.
  - [x] Padding is the depth the covered band reaches into the region's box, measured
        from the window root, so a wrapped or nested region pads once; a content-sized
        axis converges in one layout.
  - [x] Padded after each layout and before anchors resolve, inside the adaptive
        settle and its round bound.
  - [x] Authored `padding` on either widget is `E3711`.
- [x] `DisplayFeature` hinge/fold/cutout list readable from a view (§96.6).
  - [x] A view iterates and matches `env.display_features` and rebuilds when it
        changes, under hot reload and the release package.

### D4.5 — Preservation and acceptance

- [x] Focus, scroll offset and text survive switching adaptive branches when the node
      is preserved (§96.8).
  - [x] An arm set aside takes the focus and the focus scope with it, so keys never
        reach a hidden node; returning gives each back unless something else took it.
  - [x] Scroll offsets and text-editing buffers keep their values across the switch
        (the nodes keep their identity).
  - [x] Tested on the hot-reloaded and the packaged view.
- [x] §96.10 acceptance matrix as headless tests: phone portrait/landscape, tablet
      full/split, desktop narrow/wide, keyboard, safe area, fold/hinge, text scale,
      mouse vs touch.
  - [x] One view's branches and avoiding regions checked against every scenario, on
        the hot-reloaded and the packaged view.
  - [x] Scenarios: phone portrait/landscape, tablet full/split, desktop narrow/wide,
        keyboard shown/hidden, safe-area change, fold/hinge, text scale, mouse and
        keyboard vs touch.

---

## D5 — Game Profile (P1)

Goal: Quick Game and system games with compile-time determinism, snapshots and tick
timers (§104–§111).

### D5.1 — Scheduler and clock

- [x] `FixedUpdate` / `FrameUpdate` / `CollisionListener` hooks bound from `SystemIr`
      (§131), no hard-coded names.
  - [x] Native Schema traits (`NativeTrait` + hooks) registered and importable.
  - [x] `viso::game` library: the three traits and `FixedFrame` / `RenderFrame` /
        `CollisionEvent` handles.
  - [x] `implements` resolves against imported traits (`E2001`); each hook needs a
        matching `action` (`E2201`); two traits sharing a hook are `E2202`.
  - [x] Systems lower like components; `Module.systems` carries the hook→chunk table
        and survives the wire format.
  - [x] A system declares no `view` / `event` / `slot` (`E9109`); a systems-only source
        is not `E2005`.
- [x] Fixed-step loop with `tick`, `fixed_dt`, `time_scale`, `paused`, `step(n)`;
      `frame.time() = tick × fixed_dt` (§106.1).
  - [x] `Clock`: accumulator, scaled wall time, pause, `step(n)` through the same path.
  - [x] `Scheduler`: FixedUpdate per tick, collisions after, FrameUpdate per frame
        (also while paused).
- [x] `TickOverrun::{DropTime, SlowMotion}`; `game.overrun_ticks`, `game.dropped_time`.
- [x] System order and `E9101`; tick budget `E9102`.
  - [x] `@after(System)` / `@before(System)`; stable topological order; cycle `E9101`.
  - [x] Per-tick cumulative instruction budget; exhaustion skips the rest of the tick
        and reports `E9102`.

### D5.2 — Input

- [x] Edge semantics: each edge seen exactly once across 0 or many ticks per frame
      (§106.2).
  - [x] Scheduler host input (`key`/`pad`/`stick`/`touch`/`release_input`) latched
        per action; `pressed`/`released` go to the first tick after, `held` and axes
        hold for every tick of a frame, press+release in one window is both edges.
  - [x] `frame.input` property: a borrowed `InputSnapshot` (`pressed`/`released`/
        `held`/`axis`/`move_axes`) refilled each tick without allocation.
- [x] `@derive(InputAction)` enums and `@const` `InputMap` with move axes, dead zone,
      diagonal normalization (§106.3).
  - [x] Native schema: schema enums (`Key::Space`), properties, `@const` natives,
        library derives; `SchemaTy::Action` is the package's action enum.
  - [x] `@derive(..)` checked: standard or schema derive (`E2001`), schema derive only
        on a unit-only enum (`E2201`).
  - [x] One `InputMap<E>` per package (`E2202`), `E` derives `InputAction` or is
        `InputAction` (`E2201`); actions typed `E` (`E2103`).
  - [x] Compile-time evaluation of the map from literals, variants and `@const`
        natives (`E2501`, `E2112`) into the module's input schema (wire round trip).
  - [x] Radial dead zone (default 0.2) and keys+stick normalized to length ≤ 1;
        `relative_to(yaw)` for camera-relative movement.
- [x] `E9107` missing gamepad/touch path for the target platforms.
  - [x] `InputDevices` from the package targets (desktop host → gamepad, mobile
        target → touch); one warning per action and device.

### D5.3 — State tiers and determinism

- [x] `@local` state; Simulation domain = hooks plus reachable callables (§106.4).
  - [x] Hook domain declared in the schema (`FixedUpdate`/`CollisionListener`
        Simulation, `FrameUpdate` Presentation); reachability over the package call
        graph, each finding noting the hook that reaches it.
  - [x] `@local` only on a system `state` (`E9103` otherwise).
- [x] `E9103` Simulation touching Local or Presentation return values.
- [x] `E9104` non-deterministic sources in Simulation.
  - [x] Native determinism tier (`none`/`same_binary`/`cross_platform`) against
        `[game] determinism` (manifest + CLI); native tasks, `task` calls, `await`,
        `env`.
- [x] `Snapshot` trait, auto-derived for value types; `E9105`.
  - [x] Value types, records, enums, tuples and containers derive it; closures and
        handles whose type declares no snapshot do not.
- [x] Presentation commands keyed `(tick, source_system, sequence)`, not redelivered on
      replay or rollback; debug draw stripped in release.
  - [x] Deferred by the VM in Simulation hooks, discarded with a faulting hook,
        delivered after the tick in key order behind a delivery watermark
        (`rewind_to`, counters).
  - [x] Release profile lowers debug draw calls to nothing; the VM also drops them in
        release builds.

### D5.4 — Timers, snapshots, interpolation

- [x] `Cooldown` / `TickTimer` value types; `Duration` → ticks at compile time (§106.6).
  - [x] Native value types (`NativeType::value`, plain aggregates that snapshot);
        `Cooldown` (`new`/`ready`/`fire`/`remaining`), `TickTimer`
        (`every`/`due`/`rearm`/`remaining`, phase-locked rearm).
  - [x] `[game] tick_rate` (1–1000 Hz, default 60) → `TargetProfile` → module tick
        rate (wire); the scheduler clock steps it.
  - [x] `SchemaTy::Ticks` parameters: constant `Duration` (literals, `+`/`-`, scaling)
        converted exactly with rounding up; non-constant `E2501`, negative `E2112`.
- [x] Generated `GameSnapshot`, Ende encoding, `restore(snapshot(s))` equivalence
      (§106.7).
  - [x] Per-system snapshot layout (stable IDs, slots, type schema hashes, `@local`
        excluded) generated by the compiler and verified in the module.
  - [x] In-memory snapshot sharing values; canonical Ende blob, decode errors on
        corruption, snapshot hash; build hash.
  - [x] Restore by stable ID + schema (`Restored` counts), derived recomputed, local
        kept; fresh-session load and in-memory rollback replay tick for tick.
- [x] `RenderFrame.alpha`, previous/current transforms, `teleport` (§106.9).
  - [x] `frame.alpha(): F32` in `[0, 1)` from the clock accumulator.
  - [x] Previous/current entity positions; `RenderFrame.position(id)` and the host's
        `GameWorld::extract(alpha)` interpolate; `teleport` is not interpolated.

### D5.5 — Quick Game

- [ ] `QuickGame.start` / `fixed` lowering; startup transaction before the first tick
      (§105.1).
  - [x] `viso::game::quick`: `QuickGame` (`start`, `fixed`, both Simulation) and
        `QuickStart` / `QuickFrame`, views of the full profile's `GameStart` /
        `FixedFrame`; `viso::game::Startup` so the split form exists.
  - [x] The scheduler runs `QuickGame.start` as a `Startup` and `QuickGame.fixed`
        as a `FixedUpdate`, by hook identity.
  - [x] Startup transaction: every start hook before tick 0 in system order on one
        budget; a fault fails `Scheduler::new`, discarding the start's writes and
        commands; a successful start's commands land before tick 0's.
  - [x] `QuickStart.spawn` committed before the first tick and `QuickFrame.world`
        (`GameStart` / `FixedFrame` alike; `game_world.rs`).
  - [ ] Rerun on World Rebuild, not on a logic-only reload: lands with D5.7.
- [x] QuickGame and the equivalent single system give the same result on one tape.
  - [x] Per-frame states and delivered commands equal on a key and stick tape; the
        quick snapshot restores into the split form whole (`game_quick.rs`).

### D5.6 — World commands and queries

- [x] Deterministic command buffer merge and conflict policy (§108).
  - [x] `viso::math` `Vec2F32` / `Vec3F32` value types (single-precision IEEE,
        `cross_platform`).
  - [x] `viso::game::GameWorld` (borrowed) reached as `frame.world` / `cx.world` /
        `event.world`; reads are `fn`s over the committed revision, writes are
        `action`s buffered per hook and rolled back with a faulting hook; writes
        outside a Simulation hook fault.
  - [x] Merge in `(system, sequence)` order at the tick's command points (after the
        `FixedUpdate`s, after the collision listeners, after the start); `walk` /
        `jump` add, `teleport` last wins, `remove` voids later commands, `spawn`
        returns its `EntityId` at once and the body exists from the commit.
  - [x] Physics step between the two command points: character gravity, per-axis
        resolution against blocks (`on_floor`), begin-contact events between
        characters and sensors / characters in allocation order.
- [x] Stable query order, generational `EntityId`, `@derive(GameTag)` (§108.1).
  - [x] `EntityId` value (slot + generation; slot reuse bumps the generation);
        `query(tag)` / `entities()` in allocation order.
  - [x] Tags: the package's one `@derive(GameTag)` enum (`E2202` for a second,
        `E2201` past 64 variants) or `viso::game::GameTag`; `SchemaTy::Tag`.
  - [x] `CollisionEvent.first` / `second` / `other_of` as `EntityId`.
- [x] `GameSnapshot` `world` and `rng_state`: Native World snapshot/restore and the
      injected seeded RNG (§106, §106.7).
  - [x] Seeded RNG (`Scheduler::with_seed`), `world.random()` /
        `random_range(lo, hi)`, rolled back with a faulting hook.
  - [x] Snapshot `world` (shared in memory, canonical in the blob) and `rng`;
        restore resumes tick for tick, hash covers both.

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
