# Viso Tooling & Dev Runtime — T0 → T10

The rendering program (`todo_rendering.md`, F0 → A0 + the Global 1.0 DoD) is complete and
frozen. The text/font program (TF-P0 → TF-P6) is complete. What is missing is not another
layer *under* the frame — it is everything a developer touches *around* it: there is no
`viso` binary, no `tools/` directory, no `Viso.toml`, no watcher, no dev session, and two of
the three mandated DSL entry points (`component!`, `view!`) do not exist. `Viso_CLI.md`
(3378 lines) has zero implementation; `Viso_Hot_Reload.md` has its four pure compiler
stages (`plan → diff → migrate → commit`, `crates/dsl/src/hotreload/`) and nothing around
them.

This program builds the **host development loop end to end**, in the order both source
documents already fix for themselves: `Viso_CLI.md` §69–§75 (P0 → P6) and
`Viso_Hot_Reload.md` §65–§70 (Phase A → F). Both say the same thing — do the one-process
desktop loop first and do not try to solve mobile/web/network at the same time — so T0–T10
below interleave them into one strict order and defer their later phases to the named
follow-on programs at the bottom.

Construction order is strict. Each section's Done gate must be green and its contract
frozen before the next builds on it. Never let an upper section become a prerequisite of a
lower one: `T5`+ must never be listed as a completion prerequisite of `T0~T4`; the hot
reload transport must never become a prerequisite of `viso build`/`viso run`; the release
artifact must never depend on any `T5`+ code existing (AGENTS 60, `Viso_Hot_Reload.md` §1.2).

Landing points follow `Viso_CLI.md` §39/§40/§41 and `Viso_Hot_Reload.md` §54/§55:

```text
tools/cli/       viso-cli      the `viso` binary — args, output, orchestration ONLY
tools/project/   viso-project  project + config + profile + target + cache/lock model
tools/dev/       viso-dev      dev session — watcher, coalesce, connection, patch plan
crates/runtime/src/dev/        runtime side — handshake, stage, commit, snapshot (dev-only)
```

Three new workspace members, no more (AGENTS 3.3): each is an independently reusable
boundary with real dependency isolation — `viso-project` and `viso-dev` are consumed by the
CLI *and* by `viso-lsp` and a future Studio, and they carry the `clap`/`toml_edit`/`notify`
dependency tree that must never reach a framework crate. Direction is one-way and enforced:
`tools/* → framework crates` only. `viso-runtime → tools/*`, `viso-ui → tools/*`,
`viso-gpu → tools/*` and `viso-dsl → CLI argument parser` are forbidden edges
(`Viso_CLI.md` §41).

The wire protocol is the one place this could go wrong: the host side (`viso-dev`) and the
runtime side (`crates/runtime/src/dev/`) must agree on one schema, and the runtime cannot
depend on a tools crate. So the protocol *types* live in `crates/runtime/src/dev/protocol.rs`
and `viso-dev` depends on `viso-runtime` to reuse them — one definition, no cycle, and the
whole module tree disappears from release together (`Viso_Hot_Reload.md` §55).

Gate for every section: `cargo xtask check-deps` · `cargo fmt --all -- --check` ·
`cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace`.
Build/test with `CARGO_TARGET_DIR=/tmp/rust_tmp`. Commit per section (source + this file in
one commit, AGENTS 40).

---

## T0 — Project, config, profile and target model (`tools/project`)

Goal: one answer to "what project am I in, what is the resolved configuration, and what am
I building". Everything after T0 asks this crate instead of re-deriving it. No CLI, no
GPU, no compiler — a pure resolution library over `Viso.toml` and the environment.

- [ ] `tools/project/` new crate `viso-project`; workspace member; `toml_edit` dependency
      (format-preserving DOM parse, no `serde` in the tree — `viso config` must be able to
      report the *source* of every value, which a plain deserialize throws away).
- [ ] `Viso.toml` schema (`Viso_CLI.md` §38), typed and total — unknown keys are a
      diagnostic, not silence:
  - [ ] `[package]` name / bundle_id / version.
  - [ ] `[build] default_target`.
  - [ ] `[profile.dev|release|shipping]` opt_level / source_maps / strip (§38.2).
  - [ ] `[web]`, `[target.android]`, `[target.ios]`, `[export.*]` parsed and typed now even
        though their commands are deferred — an unknown-key diagnostic must not fire on a
        config the deferred phases will honor.
  - [ ] Reject `hot_reload = true` (or any synonym) under `release`/`shipping` with a
        stable config-error code, never a silently patchable release package (§38.2).
- [ ] Project discovery (§4): walk up from cwd to the first `Viso.toml`, stop at filesystem
      root; `--project <path>` overrides; workspace `members` in the root manifest.
- [ ] Config precedence (§5), with provenance recorded per value so `viso config show` can
      print where each came from: CLI flag → `VISO_*` env → `Viso.toml` target/profile
      override → `Viso.toml` project defaults → framework default.
- [ ] Artifact-kind model (§2): `build` / `package` / `export` are three different outputs
      with three different meanings; the type system must not let one be mistaken for
      another.
- [ ] Target model (§3): `Host` (resolved from the current OS — never a positional the user
      types, §1) and `Headless` (§3.4) are the T0 targets; `Ios`/`Android`/`Web*` are
      declared in the enum and return a clean "not in this build" for now (§3.2/§3.3).
- [ ] Build mode ↔ dev-runtime linkage is fixed by the profile, not configurable
      (§38.2): `dev → Dev Runtime present`, `release|shipping → Dev Runtime absent`.
      Expose it as a resolved fact other sections read; T5 turns it into a cargo feature.
- [ ] `BuildId` (§46) — a content-addressed id over {project fingerprint, target, profile,
      toolchain} that the T5 handshake compares; `ProjectFingerprint` over the source graph.
- [ ] Cache layout (§45) and project lock (§44) under `target/viso/`: build-cache lock,
      package-output lock, dev-session lock per target — advisory file locks, so two
      `viso run`s on one project cannot corrupt one artifact, while two *targets* stay
      concurrent.
- [ ] Extend `cargo xtask check-deps` to cover `tools/*`: the crate→directory mapping is
      currently `viso-x → crates/x`, which cannot see a tools crate; add an explicit path
      per allowlist entry, admit `viso-project`/`viso-dev`/`viso-cli`, and add the §41
      reverse check — no `crates/*` manifest may name a tools crate.
- [ ] Tests: discovery from a nested dir / no-manifest / explicit `--project`; precedence
      with a flag and an env var and a profile override all in play; every provenance
      answer; unknown-key and release-hot-reload diagnostics; lock contention;
      `BuildId` stability and its change on each input.
- [ ] Gate green → commit T0.
- [ ] FREEZE T0: `ResolvedConfig`, provenance, `Target`, `Profile`, `BuildId`,
      `ProjectFingerprint`, cache/lock paths.

---

## T1 — CLI foundation (`tools/cli`, `Viso_CLI.md` §69 P0)

Goal: the `viso` binary exists, its grammar is the product contract, and both of its output
modes are machine-checked. Nothing here initializes a GPU, a runtime, a compiler database
or the network (§64).

- [ ] `tools/cli/` new crate `viso-cli`, `[[bin]] name = "viso"`; `clap` dependency (§42
      explicitly sanctions a mature parser crate); module layout per §39 — `command/*.rs`
      is orchestration only, no domain implementation.
- [ ] Full command tree parsed and helped, including the deferred ones, each failing with
      exit 3 + a "not in this build" diagnostic rather than an unknown-subcommand error
      (§1) — the grammar is frozen now so later phases add behavior, not syntax.
- [ ] Global options (§6) accepted before *and* after the subcommand and normalized to one
      value (§4.1): `--project`, `--json`, `--quiet`, `--verbose`, `--target`, `--profile`.
- [ ] Stable exit codes (§7): 0 success · 1 diagnostics · 2 usage · 3 environment ·
      4 build · 5 runtime/test · 6 package · 7 internal · 130 interrupt.
- [ ] Human output (§52/§53/§54): help style, error message shape (what happened / where /
      what to do), progress that degrades to plain lines on a non-TTY.
- [ ] `--json` is a JSON-Lines *event stream*, not a final object (§34): one valid JSON
      object per line over `viso_ende::JsonWriter`; envelope `{type, schema, timestamp_ms,
      session_id, payload}` (§35); event types `progress|diagnostic|artifact|device|test|
      snapshot|profile|server|log|summary` (§36); exactly one terminal `summary` per finite
      command (§36.3); stdout carries protocol only, stderr only pre-protocol launcher
      failure (§37).
- [ ] Cancellation (§43): one token threaded through every long command; Ctrl-C leaks no
      child process, socket, or temp directory; exit 130.
- [ ] Non-TTY never prompts (§55); `--quiet`/`--verbose` semantics (§6.2/§6.3).
- [ ] `viso config show|get|path|validate` (§10) over T0 — the first real command, and the
      acceptance of T0's provenance.
- [ ] `viso completion <shell>` (§42 completion metadata) and `viso clean`.
- [ ] Tests: parser golden tests for the whole grammar incl. aliases (§59, §68); command
      integration tests (§60); JSON contract tests — every event validates against the
      envelope and every command ends in exactly one summary (§62); Ctrl-C/cleanup (§63);
      `viso --help`/`--version` touch no heavy subsystem (§64).
- [ ] Gate green → commit T1.
- [ ] FREEZE T1: command grammar, global options, exit codes, JSON envelope + event types.

---

## T2 — Three DSL entry points, one pipeline (AGENTS 21.5)

Goal: close a hard MUST that is currently unmet. `ui!` exists; `component!` and `view!` do
not. All three must share schema, name resolution, type/effect checking, Typed HIR,
Reactive/UI/Shader IR and diagnostics — the frontend already does, so this is an entry
surface, not a second compiler.

- [ ] `component!` proc-macro in `viso-ui-macros` on the existing `ComponentDecl` grammar
      entry (`crates/dsl/src/syntax/grammar/decl.rs`), emitting the same `viso_ui` builder
      shape `ui!` emits.
- [ ] `view!("path.vs")` proc-macro: resolve the path relative to the invoking file, compile
      it through the normal module/file frontend, and register the file as a compile
      dependency so editing the `.vs` rebuilds the Rust crate (AGENTS 21.5: `view!` compiles
      an external `.vs` through the file frontend, not an inline copy of it).
- [ ] Diagnostics from all three map back to real spans — inline macro spans for
      `ui!`/`component!`, `.vs` file + line/col for `view!`.
- [ ] `.vs` files need no per-file `language`/`module` header (AGENTS 21.5.3): module path
      derives from package + source path, language version from `Viso.toml`.
- [ ] Contract test: three equivalent sources — one `ui!`, one `component!`, one `.vs` via
      `view!` — produce the *same* Typed HIR and UI/Binding IR. This is the assertion that
      keeps them one pipeline instead of three dialects.
- [ ] Gate green → commit T2.
- [ ] FREEZE T2: the three entry-point parse contracts and their shared lowering.

---

## T3 — Language commands (`Viso_CLI.md` §71 P2, language subset)

Goal: the `.vs` toolchain is reachable from the command line and from a machine. The
engine already exists in `viso-dsl` + `viso-lsp`; this is orchestration plus one thing that
does not exist yet — a stable diagnostic code registry.

- [ ] Diagnostic code registry (AGENTS 30, §19): every diagnostic carries a stable code,
      severity, primary span, related spans, notes, and safe fixes; codes are frozen once
      published and tested against a golden list.
- [ ] `viso fmt` + `--check` (§16) over `viso_lsp::format`; §16.3 — the formatter runs on
      the real parser's lossless CST, never a regex pass.
- [ ] `viso check` (§17) with fast default (§17.2), `--target` (§17.1) and `--watch`
      (§17.3); diagnostics in both human and JSON form; exit 1 on error.
- [ ] `viso explain <code>` (§19) from the registry.
- [ ] `viso schema` (§18) query forms over the component/native schema — the structured
      interface IDEs and AI use instead of parsing help text (§56/§57/§58).
- [ ] `viso dump` (§20): CST / AST / HIR / UI IR / binding IR, each a stable machine shape.
- [ ] `viso lsp` (§21): delegate to `viso-lsp` over stdio; one engine, not a second one.
- [ ] Tests: golden diagnostics with codes and spans; `fmt --check` idempotence on the
      repo's own `.vs` fixtures; `dump` shape stability; `schema` query answers; malformed
      input recovery (AGENTS 35).
- [ ] Gate green → commit T3.
- [ ] FREEZE T3: diagnostic codes, `dump`/`schema` machine shapes.

---

## T4 — Host build / run / test loop (`Viso_CLI.md` §70 P1)

Goal: the minimum daily-development closed loop, with no hot reload yet — `viso new` to a
running app, and a release build that carries no compiler (AGENTS 21.6). This is the gate
that must be green before any of T5's machinery is allowed to exist.

- [ ] `viso new <name>` (§8) + templates (§8.1) + `--here` (§8.3): generates a project that
      builds and runs unmodified, with `Viso.toml`, a `.vs` view, and a `main.rs` using only
      `viso::prelude::*` and `viso::run::<App>()`. §8.4 acceptance.
- [ ] `viso doctor` (§9): explains environment gaps, changes nothing on its own (§9.3),
      `--json` (§9.4).
- [ ] `viso build` (§14): profiles (§14.1) distinct from targets (§14.2); artifact summary
      (§14.4) and manifest; release/shipping emits the compact AOT package via
      `viso_dsl::aot::build_package` so startup parses no `.vs` (AGENTS 21.6) and the
      compiler is absent from the artifact.
- [ ] `viso run` (§13.1): desktop host resolved from the current OS, never a positional
      (§1); app arguments after `--` (§13.4); `--no-hot-reload` accepted now and a no-op
      until T5 (§2.1 HR); Ctrl-C (§13.8).
- [ ] `viso test` (§22) incl. headless UI (§22.2) and filters (§22.4); the headless backend
      is the CI path (AGENTS 66).
- [ ] Phase timings surfaced under `--verbose` (§65): change detection, DSL compile, Rust
      rebuild — the stages T5 will optimize, measured before T5 exists.
- [ ] Tests: `viso new` output builds and runs headless; release artifact contains no
      compiler path and boots from the AOT package with the frontend absent; build/run/test
      exit codes; artifact manifest shape.
- [ ] Gate green → commit T4.
- [ ] FREEZE T4: `viso new` template contract, artifact manifest, AOT package boot path.

---

## T5 — Dev Runtime and Hot Reload Phase A (`Viso_Hot_Reload.md` §65)

Goal: edit a `.vs` property, see it live, without restarting the process — and prove the
whole apparatus is absent from a release binary. The four pure stages already exist
(`plan → diff → migrate → commit`); this section builds the session, the transport, the
frame-boundary commit and the last-good loop around them.

- [ ] `crates/runtime/src/dev/` (§55): `mod.rs`, `handshake.rs`, `patch.rs`, `stage.rs`,
      `commit.rs`, `snapshot.rs`, `diagnostics.rs`, behind a `dev` cargo feature that
      `viso build --profile dev` enables and release/shipping cannot (§1, §1.1 — no runtime
      re-enable switch exists to be flipped).
- [ ] `crates/runtime/src/dev/protocol.rs`: the single wire schema both sides use — Ende
      binary, bounded lengths, stable message tags, canonical ID encoding, no field-name
      strings on the hot protocol (§33, §35).
- [ ] `tools/dev/` new crate `viso-dev` (§54): `session.rs`, `watcher.rs`, `coalesce.rs`,
      `connection.rs`, `protocol.rs` (host codec over the runtime's schema), `patch_plan.rs`,
      `targets/desktop.rs`; `notify` dependency for filesystem events.
- [ ] Host owns watching and compilation; the runtime never sees source (§3, §9).
- [ ] Watch scope (§6) and the fact that a filesystem event is not a semantic change (§7):
      debounce (§7.1) then content hash (§7.2) — an editor's save-twice must produce one
      patch, and a touch with identical bytes must produce none.
- [ ] Dev session identity (§4, §4.1) and the desktop connection topology (§5) — a local
      socket under the T0 dev-session lock.
- [ ] Handshake (§34): `HostHello{protocol_version, dev_session_id, project_fingerprint,
      expected_build_id}` / `RuntimeHello{protocol_version, runtime_session_id, build_id,
      current_revision, schema_fingerprint, capabilities}`; an incompatible version is
      rejected clearly with a rebuild request — the decoder never guesses (§34).
- [ ] `PatchBundle` (§35) carrying the *typed* patch the host planned, plus revision rules
      (§36): apply only when `base_revision == current_revision`, else
      `NACK_REVISION_MISMATCH` and the host regenerates or requests warm restart. No blind
      out-of-order apply.
- [ ] Staging then atomic commit (§38, §39): nothing mutates the live tree during
      validation, and the commit lands on a frame boundary, never mid-frame.
- [ ] `PatchAck{revision, applied_domains, scoped_resets, timings}` /
      `PatchNack{base_revision, candidate_revision, stage, diagnostic_codes,
      last_good_revision}` (§37).
- [ ] Last-good as a first-class contract (§41) and the error loop (§42): an invalid `.vs`
      leaves a running, interactive app at its last-good revision and shows the diagnostic;
      the next valid save recovers with no restart.
- [ ] `viso run` wires it (§2, §13.5, §13.6): hot reload belongs to the dev build only;
      `--no-hot-reload` (§2.1) produces a dev build with the session not started.
- [ ] Release absence test (§64 HR / AGENTS 60): a release/shipping build contains no dev
      transport, no patch-apply entry point, no DevSnapshot endpoint, and the steady-state
      frame path has zero hot-reload branches (§1.2) — asserted, not asserted-by-review.
- [ ] Patch latency and allocation targets measured, not claimed (§48, §49, §50; AGENTS 7.3).
- [ ] Tests (§61, §62): property-only patch does not restart the process; invalid `.vs`
      keeps last-good; revision mismatch NACKs; malformed patch cannot corrupt last-good
      (§ protocol DoD); debounce/hash coalescing; release-absence.
- [ ] Gate green → commit T5.
- [ ] FREEZE T5: protocol version, message tags, handshake, revision rules, ACK/NACK.

---

## T6 — Hot Reload Phase B: structural and state patch (`Viso_Hot_Reload.md` §66)

Goal: insert, remove and reorder nodes live while state, focus and scroll survive by
identity. The pure planners exist (`diff.rs`, `migrate.rs`); this section puts them on the
wire and pins their semantics.

- [ ] Structural insert / remove / reorder across the protocol, aligned by `StableKey` /
      `NodeKey` (§11, §12, §13; AGENTS 21.8).
- [ ] `SymbolId` linking so a renamed-but-same symbol is a patch, not a reset (§11).
- [ ] State compatibility classes (§14): exact-compatible preserved, explicitly convertible
      converted, incompatible reset — and the reset is *scoped*, never global.
- [ ] UI ephemeral state (§15): focus, scroll, selection, composition preserved where the
      structure allows, and counted precisely where it could not be.
- [ ] The three outcome classes are explicit and reported (§8): `PATCH`,
      `PATCH_WITH_SCOPED_RESET`, `WARM_RESTART_REQUIRED`.
- [ ] Tests (§62 HR): structural insert preserves sibling state; type change scopes the
      reset; focus and scroll preservation; a change that cannot be patched classifies as
      warm restart instead of silently dropping state.
- [ ] Gate green → commit T6.
- [ ] FREEZE T6: outcome classes, state-compatibility rules, scoped-reset boundary.

---

## T7 — Shader / resource / font hot reload (`Viso_Hot_Reload.md` §67 Phase C)

Goal: a shader or an asset edit reloads with the same transactional guarantee, and a failed
compile never reaches the GPU.

- [ ] Shadow compile (§19): the candidate shader compiles and validates in the background
      while the old pipeline keeps drawing.
- [ ] Interface compatibility check (§20): an incompatible instance/uniform interface is a
      rejection, never a reinterpretation of GPU memory (AGENTS 18, 30).
- [ ] GPU-safe boundary swap (§21): the pipeline exchanges between frames, never inside an
      encoded pass.
- [ ] Resource revision (§22) and font revision (§23): an image/font/asset updates on its
      own revision without flushing the whole UI or glyph cache.
- [ ] Tests: invalid shader keeps the old pipeline and the app interactive; valid shader
      swaps with no visual discontinuity beyond the intended change; a font swap does not
      invalidate unrelated atlas pages.
- [ ] Gate green → commit T7.

---

## T8 — Rust warm restart (`Viso_Hot_Reload.md` §68 Phase D)

Goal: editing Rust does not lose your application state, and never kills a working app to
find out the new one does not compile.

- [ ] Cargo incremental coordinator (§25): the host drives the rebuild and reports its
      phase timings (§65 CLI).
- [ ] The old app keeps running for the whole rebuild; it is killed only after the build
      succeeds (§25, anti-pattern B.5).
- [ ] `DevSnapshot` (§29–§32): typed state only — no memory dump, and no raw OS/GPU/runtime
      handle ever enters a snapshot (§30); explicit schema (§31) and storage rules (§32).
- [ ] Relaunch and restore (§26): compatible state restored, incompatible state reported.
- [ ] No arbitrary machine-code injection (§24) — warm restart is a process restart with
      typed state carry-over, and nothing else.
- [ ] Tests: build failure leaves the old app running and reports diagnostics; successful
      rebuild restores compatible state; a snapshot containing a handle is impossible by
      construction.
- [ ] Gate green → commit T8.

---

## T9 — Observability commands (`Viso_CLI.md` §71/§75, host subset)

Goal: the counters the render and text programs already expose become reachable from the
command line and from a machine, without a GUI.

- [ ] `viso snapshot` capture / compare / update (§23) over the headless backend — the
      golden-image workflow AGENTS 35 asks for, driven by the CLI instead of a test harness.
- [ ] `viso inspect --query` headless (§24.3) over the AGENTS 62 introspection surface:
      node tree, layout box, dirty reasons, bindings, semantics, primitive ranges, batches.
- [ ] `viso profile` (§25) over the existing `FrameStats` / text / dev-session counters
      (AGENTS 61): metrics (§25.2) and trace output (§25.3).
- [ ] Tests: snapshot compare detects a one-pixel change and passes on an identical frame;
      every `inspect` query answers from the real runtime, not a mirror.
- [ ] Gate green → commit T9.

---

## T10 — Example ladder (AGENTS 48)

Goal: the public API contract, demonstrated. Only `01-counter` and `hello_world` exist
today, and `01-counter`'s own doc comment is stale. Each example is also the acceptance
surface for something built above.

- [ ] `00-minimal`, `02-layout`, `03-state`, `04-navigation`, `05-async`, `06-list`,
      `07-text-input`, `08-adaptive`, `09-accessibility`, `10-custom-shader`, `99-full-app`.
- [ ] Every example uses `viso::prelude::*` and public API only — no internal-crate import
      standing in for a missing public one (AGENTS 48).
- [ ] Each runs under `viso run`, hot-reloads a `.vs` edit, and has a headless test.
- [ ] Refresh `01-counter`'s stale doc comment (it claims there is no text control;
      `crates/widgets/src/text.rs` has had `Label` since `863bf22`).
- [ ] Gate green → commit T10.

---

## Program Done

- [ ] `Viso_CLI.md` §76 DoD, Project / Develop / Language / Test-Debug / Automation
      sections — all green (Mobile / Web / Delivery are the deferred programs below).
- [ ] `Viso_Hot_Reload.md` §71 DoD, Dev-only / UI / Shader / Resources / Rust / Protocol
      sections — all green (Game and Mobile/Web are deferred below).
- [ ] One executable acceptance file per document, the way
      `crates/render/tests/render_contract_1_0.rs` states the rendering DoD: the outcome,
      not the mechanism — the mechanism contracts live in each section's own tests.
- [ ] Every core command supports `--json`; exit codes stable; non-TTY never prompts; CLI
      duplicates no compiler/build/packager implementation (§76 Automation).

---

## Deferred — named, with the reason

Not "later maybe": each is blocked on something concrete, and each is the next program in
its own direction.

- **CLI P3 — mobile development environment** (§72; `viso android|ios …`, HR Phase E):
  needs an Android SDK/emulator and an Apple simulator to verify, and needs a Vulkan
  backend and an iOS Metal surface path in `viso-gpu`, which do not exist
  (`crates/gpu/src/` has `metal.rs` and `headless.rs` only).
- **CLI P4 — web** (§73; `web-gpu|web-dom|web-hybrid`, `viso serve`, HR Web transport):
  needs a WebGPU backend in `viso-gpu`.
- **CLI P5 — delivery and export** (§74; `package`, artifact manifest signing,
  `export html`, `export solid`): downstream of a working build on more than one target;
  signing cannot be verified here.
- **CLI P6 / HR Phase F — Studio and multi-session** (§75, §70 HR): a client of the
  introspection surface T9 builds, not a prerequisite of it.
- **HR Phase C game systems** (§16–§18): fixed-tick-boundary patching needs the Game
  System scheduler surface (AGENTS 21.5.5), which is schema work not yet started.
- **Tier-1 backend breadth** (AGENTS 65.1: Windows/D3D12, Linux/Vulkan, Android/Vulkan,
  Web/WebGPU): the single largest remaining program, and the prerequisite of P3/P4 above.
  `crates/platform/src/backend/windows.rs` (309 lines) and `x11.rs` (327 lines) are
  seams, not backends; `macos.rs` is 1658.
- **Widgets / accessibility 1.0** (AGENTS 15, 33, 51): `crates/widgets` is 19745 lines with
  2 integration-test files; the OS accessibility bridge is unwritten.
- **`crates/services`** (AGENTS 24): still the 32-line Phase 0 skeleton — files, share,
  permissions, camera, location, notifications, secure storage, haptics, network.
- **Migration canary** (AGENTS 38.6): `tools/migrate/` and a real Makepad crate kept as a
  measurable baseline.
