# Viso CLI — C0 → C1 → C2 → C3 → C4 → C5 → C6

> The N9 sweep of 2026-10-29 moved this file's open lines to `todo_next.md`: the device runs to
> N8, the rest to its Backlog. This file is the record of what was built.

Construction order follows `Viso_CLI.md` Part XVIII (§69–§75): C0 is the foundation
(P0), C1 the host development loop (P1), C2 compiler tooling (P2), C3 the mobile
development environment (P3), C4 web (P4), C5 delivery and export (P5), C6 full
observability (P6). C7 (cross-cutting contracts: JSON protocol, reliability, UX, AI,
performance, the CLI's own tests) grows with every phase, and C8 is the Definition of
Done (§76). A later phase never becomes a prerequisite of an earlier phase's slice.
Work one verifiable sub-unit at a time; a surface change (a command, a flag, an event,
a diagnostic code, an exit code) updates `Viso_CLI.md` in the same change, and an
architecture decision (a new tooling crate, a service boundary, a protocol) adds an
ADR. A perf claim needs a release measurement; otherwise it is labeled a hypothesis.

Landing points follow the dependency rules (§39–§41):

- `tools/cli` parses, resolves, orchestrates and renders; it never reimplements a
  service. `command/*.rs` only orchestrates; each command gets its own file as listed
  in §39 (`run.rs` and `serve.rs` split, `device.rs` becomes `android.rs`/`ios.rs`).
- The project model (discovery, `Viso.toml`, precedence, targets, cache layout, locks,
  build ids) lives in `viso-project`; the compiler, its diagnostics registry, schema
  and IR dumps in `viso-dsl`; app wrapping, signing and the artifact manifest in
  `viso-package`; build orchestration, the dev session, the inspector client, device
  services, toolchains, delivery and deployment in `viso-tooling` (`tools/tooling`),
  which the CLI and Studio share and which reports through the front end's `Reporter`.
- The `.vs` formatter is a shared service: the CLI and `viso-lsp` call the same
  function, and the CLI does not depend on the LSP server.
- `viso-dsl` never depends on the CLI's argument parser; no framework crate depends on
  `tools/*` (`cargo xtask check-deps`).
- Code comments and commit messages carry no section, ADR or phase numbers and no
  version-history notes.

Reference read first (memory rule): makepad `tools/cargo_makepad/src/main.rs` (command
surface), `android/sdk.rs` and `android/mod.rs` (SDK component download and expansion),
`apple/mod.rs` and `apple/sdk.rs` (simulator and signing lookup), `wasm/mod.rs` and
`wasm/compile.rs` (web build, strip, split, brotli), `check/mod.rs` (cross-target
check), `shell.rs` (process execution), `studio.rs`. Take: a pinned, per-host component
URL set; extracting only the files a build needs out of each SDK archive; one shell
helper that names the tool and what it said on failure. Avoid: unverified downloads
(no checksum, no atomic install), a hand-rolled argument parser with free-form
`--key=value` scanning, help printed with `println!` instead of generated from the
parser, interactive steps with no non-TTY path, `install-toolchain` mutating the host
without a dry-run or a report.

Gate for every section: `cargo xtask check-deps` · `cargo fmt --all -- --check` ·
`cargo clippy --workspace --all-targets -- -D warnings` ·
`cargo clippy -p viso-cli --all-targets -- -D warnings` · `cargo test -p viso-cli` ·
`cargo test -p viso-project` · `cargo xtask check-release-absence` (host, and
`--target web|ios-sim|android` where a phase touches those builds).

---

## C0 — CLI foundation (§1, §4–§7, §42, §52, §53, §67–§69)

Goal: `viso --help`, `viso --version` and `viso config show` work in any directory,
every option of §6 parses before or after the subcommand, and every failure maps to
one stable exit code and one stable diagnostic code.

### C0.1 — What exists

- [x] clap parser (`args.rs`): subcommands, globals accepted before or after the
      subcommand, generated help, usage errors exit 2; under `--json` a usage error is
      a `diagnostic` (`CLI_USAGE`) and a `summary` on stdout.
- [x] `--help`/`--version` exit 0 before any project or compiler work.
- [x] Globals `--project`, `--quiet`/`-q`, `--json`.
- [x] Project discovery (`viso-project::discovery`): upward search for `Viso.toml`,
      `--project` as a directory or a manifest path, workspace members (C0012).
- [x] `Viso.toml` loader with spans and config diagnostics C0001–C0012
      (`viso-project::diag`), rendered with the manifest line.
- [x] Config precedence and provenance (`viso-project::config`): flag > `VISO_*` env >
      profile > manifest > default, every value carrying its `Origin` (env origins
      name the variable, manifest origins carry a span); env injected for tests.
- [x] Target model (`viso-project::target`): host implicit, `headless`, `ios`,
      `android`, `web-gpu`, `web-dom`, `web-hybrid`; unavailable targets are C0011.
- [x] Cache layout, build ids and locks (`viso-project::cache`, `fingerprint`):
      `target/viso/{build/<id>,cache/<kind>,dev,generated,traces,locks}`, `dist/<target>`,
      32-hex `BuildId`, non-blocking lock with owner and age.
- [x] Human and JSON output (`output/`): event envelope (`schema`, `schema_version`,
      `seq`, `type`, `timestamp_ms`, `session_id`, `command`, `payload`), stdout for
      events/results, stderr for human progress and logs.
- [x] Exit codes 0–5 and 130 (`command/mod.rs`).

### C0.2 — Parser and globals

- [x] Config model aligned with §3.5/§5/§38: the target is the command's positional
      only (no `VISO_TARGET`, no `[build] default_target`), the flag layer is
      `--profile`, `[target.android] backend` removed (backends are fixed per
      target), an unknown key is an error (C0004, exit 1).
- [x] The full top-level tree of §1 grouped in help (§52) — done in C0.5; each
      command still lands with its own section, never as a stub.
- [x] Remaining globals (§6): `--verbose`/`-v` (resolved config, the Cargo command
      line; `log{level:"debug"}` under `--json`), `--color auto|always|never`
      (`NO_COLOR`, `TERM=dumb`, non-terminal stderr → plain; colors human
      diagnostics and is passed to Cargo), `--offline`, `--locked`, `--jobs <N>`
      (N ≥ 1), `--profile dev|release|shipping`, `--target-dir <path>`; `--quiet`
      with `--verbose` is exit 2.
- [x] `--profile` is the flag layer of `config::resolve`; a dev-only command
      (`run`, `serve`) refuses another profile (`CLI_USAGE`); `--target-dir` moves
      `Layout`'s `target/` (relative to the current directory) and Cargo's
      `CARGO_TARGET_DIR`; `--offline`/`--locked` reach `cargo metadata` and
      `cargo build`, `--jobs`/`--color` reach `cargo build`. SDK downloads and
      templates take `--offline` when they land (C1.2, C3.2).
- [x] Positional target vs `--target` (§3.5): `--target` on a positional-target
      command is `CLI_USAGE` with the positional spelling in its note
      (`args::takes_positional_target`; each such command joins the list as it
      lands).
- [x] Aliases only `-h -V -v -q` (§68): `game record -o` became `--output`; a parser
      test walks the command tree for short options and aliases.
- [x] `context.rs`: one `Context` per invocation (globals, the `VISO_*`
      environment captured once); `project`, `load`, `load_dev`, `layout`,
      `cargo_args` give every command the same project, configuration, paths and
      Cargo options, diagnostics reported once in their file.
- [x] `error.rs`: every exit code and every CLI diagnostic code declared once, each
      code bound to its exit (`Code { name, exit, summary }`, `ALL`, `by_name`);
      `Output::failure(code, …)` returns the exit, so a failure cannot take another;
      a code under another service's name (`E*`, runtime faults) goes through
      `Output::error`. Tests: one exit class per prefix, unique names.
- [x] Exit codes 6 (`PACKAGE`) and 7 (`INTERNAL`); a panic in a command is caught,
      reported as `INTERNAL_PANIC` (exit 7) with its message and a "please report"
      note, and the stream still ends with its `summary`. `GAME_ANSWER_INVALID`
      (an undecodable answer) is a protocol violation, exit 7.
- [x] `run.rs` → `run.rs` + `serve.rs`. `command/device.rs` stays the shared profile
      manager; `android.rs` and `ios.rs` arrive with `list/use/doctor` (C3.2, C3.4).
- [x] Section, ADR, AGENTS and phase references stripped from CLI, `viso-project`
      and test comments.

### C0.3 — `viso config` (§10)

- [x] `viso-project::settings`: every key of the schema in a fixed order (`KEYS`) —
      the build keys through every layer, `profile.<name>.*` as a build with that
      profile gets them, every other key the manifest's value or the framework's
      default (`settings::defaults`, checked against the DSL's and the packager's own
      constants), `Value::Unset` where neither exists.
- [x] `config show`: aligned human table (key, value, origin with its place:
      `env VISO_OPT_LEVEL`, `Viso.toml [profile.release]:5:9`, `Viso.toml:2:8`);
      JSON `result{key: {value, origin, from?}}` with typed values; `--profile`
      chooses the current profile.
- [x] `config get <key>`: `key = value (origin)`, JSON `{key, value, origin, from?}`;
      an unknown key is `CLI_USAGE` with the nearest key.
- [x] `config path`: the `Viso.toml` in use, as §10.3 defines it.
- [x] `config validate`: every manifest and environment problem at once (C0001–C0012),
      exit of the first error, `result{path}` when valid. Manifest checks added:
      bundle ID, version, team ID, minimum OS, export directories, package manager,
      web-only `web.default_target` (C0007 at the value).
- [x] No write subcommand (§10.5); a parser test asserts `config set` is a usage error.
- [x] Integration test (`tests/config.rs`): flag, environment, profile table and
      manifest in play at once, each key's origin; `get` of an unknown key; `path`
      from a nested directory; `validate` with manifest and environment errors.

### C0.4 — `viso completion` (§42.1)

- [x] `completion bash|zsh|fish|powershell` (`clap_complete` dynamic engine): the
      script calls `viso` back with `VISO_COMPLETE` set, answered first thing in
      `main`; no project read; `--json` is `CLI_USAGE`; an unknown shell exit 2.
- [x] Candidates from the grammar (subcommands, options, targets, profiles, colors,
      presets) and, from disk at completion time, configuration keys, device
      profile IDs (`--device`, `start|stop|delete`) and `tests/game/` scenarios;
      path hints on `--project`, `--target-dir`, `--tape`, `--output`. Never builds,
      boots or reaches the network.

### C0.5 — Help and errors (§52, §53)

- [x] The top-level command list grouped as PROJECT / MOBILE DEV ENVIRONMENT /
      DEVELOP / LANGUAGE / TEST / DEBUG / DELIVERY / MAINTENANCE (§1, §52; `game`
      added to §1 under TEST / DEBUG), names and purposes read from the parser; a
      test holds every top-level command in exactly one group (`help.rs`).
- [x] Help style (§52): purpose, usage, examples, arguments, options, global
      options, notes, in that order, one template for every page; two or three
      examples per command at every level, each parsed by a test; `--help` is the
      `-h` page (values inline); no `help` subcommand; global options under
      `Global options:` after the command's own.
- [x] Error format of §53 everywhere: `error[CODE]: message`, location, `= note:`,
      `= help: run `<command>``; in JSON the next command is a fix without edits
      (`applicability: maybe-incorrect`), the one §138 shape (`Output::failure_next`).
      Usage errors too: the parser's message, tips, possible values and usage
      become a `CLI_USAGE` diagnostic in human text as in JSON, the command's
      `--help` next; device, serve, game and config suggestions moved from notes.

### C0.6 — Acceptance

- [x] Golden tests (`tests/golden.rs`, `tests/golden/`, `BLESS=1` rewrites): every
      help page reached through the pages themselves, one usage error of each kind
      (each exit 2), `viso config show` of a fixed project; `viso --version` by
      shape: version and commit (`build.rs`), language, event schema, diagnostic
      schema and dev protocol versions, constants only (§64), `result` under
      `--json`.
- [x] `viso config show --json` in a project with flag, env, profile and manifest
      layers in play reports the winning layer for each key (`tests/config.rs`).

---

## C1 — Host development loop (§8, §9, §13, §14, §16, §17, §22, §45.1, §70)

Goal: `viso new app && viso check && viso test ui && viso run` is a daily loop on the
desktop host, `viso build` writes a reported artifact, `viso doctor` explains every
gap, and CI can run all of it headless.

### C1.1 — What exists

- [x] `viso check`: discovery, manifest, all `.vs` sources, diagnostics with file,
      line, column and fixes, nested-directory start (tests in `tests/check.rs`).
- [x] `viso run` on the host: dev session lock, watcher before build, Dev artifact via
      cargo with `hot-reload`, loopback dev channel with token, patches, warm restart,
      `--no-hot-reload`, `--` app args, Ctrl-C two-stage cleanup (exit 130).
- [x] `viso test game [<scenario>]`: deterministic tapes under `tests/game/`.
- [x] `viso game peek|record` against a running session.
- [x] `viso game replay [--seconds S]`: the last seconds of play again on the
      reloaded code (`GAME_NO_HISTORY`).

### C1.2 — `viso new` (§8)

- [x] `ui!`, `component!`, `view!` and `asset!` expand to paths through the facade
      (`viso::__private`), so a project that depends only on `viso` builds its
      views; the examples drop their internal-crate dependencies.
- [x] Templates `app`, `game`, `web`, `library` embedded in the binary (no network);
      each a Cargo package depending on `viso` at the CLI's version, plus
      `Viso.toml`, `src/*.vs`, `i18n/en.toml` (every text through `tr`, so strict
      checks are clean), `assets/`, `.gitignore`; `tests/ui/` for the binaries and
      `tests/game/` for the game; `library` has no binary and no UI scenarios.
- [x] Options `--template`, `--name` (validated as a Cargo name, `NEW_NAME_INVALID`
      exit 2 with a usable `--name` when the directory gave it), `--edition`
      (2021 | 2024), `--no-git` (default `git init` unless inside a work tree; no
      Git is a `NEW_GIT` warning), `--force-empty-dir` (fills a directory with
      files, never replaces one; a file the project writes is `NEW_PATH_TAKEN`).
- [x] `viso new <path>` refuses a non-empty directory (`NEW_PATH_TAKEN`, exit 2,
      lists what is there, the fix adds `--force-empty-dir`); `viso new .` takes
      the directory's name; missing parents are made.
- [x] Generated files written atomically: a new directory is staged beside it and
      renamed; an existing one is staged inside and filled file by file; a
      failure or Ctrl-C removes everything made (`NEW_WRITE` exit 7, or 130).
- [x] JSON `progress`, `result{path,template,files}`; human output prints the next
      commands. Help (PROJECT group, examples, notes), completion from the grammar,
      golden help updated.
- [x] Acceptance: for each template `new → check → build headless → test` passes
      (`tools/cli/tests/new.rs`, offline, `[patch.crates-io]` to the workspace's
      `crates/viso`); plus layout, refusal, force, `.`, name, Git and failed-write
      tests.
- [x] Headless backend: a closed window takes no more redraws, so a view running a
      game (a frame client asking for a frame every frame) ends its UI scenario
      instead of spinning forever.
- [x] `--framework <path>`: a new project depends on a checkout's `crates/viso` (or
      the crate itself) by a path relative to it; another path is `NEW_FRAMEWORK`.
- [ ] A published `viso` on a registry: until then a project made without
      `--framework` resolves only with a `[patch]` to the framework's source.

### C1.3 — `viso doctor` (§9)

- [x] A `Check { id, title, status: ok|warn|error|skip, detail, fix? }` list built
      from probes: Rust toolchain and targets, cargo, the project (manifest, members,
      language version), GPU backend availability on the host, git; one
      `doctor_check` event each, human table, exit 3 on any error. (Statuses are
      §9.4's `ok|warn|missing|error`; a check that does not apply is left out.)
- [x] `doctor <target>` adds that target's probes (delegating to `android doctor`,
      `ios doctor`, the web probes of C4) and nothing else. (The probes live in the
      doctor; `android doctor`/`ios doctor` (C3.2, C3.4) reuse them.)
- [x] `--fix` performs only fixes that are local and reversible (`rustup target add`,
      creating `target/viso/`), prints each before running it, and never downloads an
      SDK (that is `android use` / `ios use`; §9.3). (`target/viso/` needs no fix:
      every command creates it; the doctor checks that it can be written.)
- [x] Probes run in parallel with a per-probe timeout; a hung tool is `error` with the
      timeout, not a hung doctor.
- [x] Point `android-sdk` and `ios-runtime` gaps at `viso android use` / `viso ios use`
      once C3.2 and C3.4 land; until then the message says what to install.
      (`android-sdk`, `android-tools` and every no-SDK failure suggest
      `viso android use <level>`; `ios-runtime` suggests `viso ios use <v>`.)

### C1.4 — `viso fmt` (§16)

- [x] The formatter moved out of the LSP server into `viso_dsl::format` (no new
      edge; ADR 0018 amended); `viso-lsp` re-exports it, its tests moved with it.
- [x] `viso fmt [<path>...]` formats every project `.vs` under `src/` and `tests/`
      (or the given files and directories), writes only files whose text changed,
      atomically (`atomic::write`: sibling file, rename, permissions kept).
- [x] `--check`: no writes, lists unformatted files with the first differing line,
      exit 1; JSON `result{file, changed, line}` per file (§16.4).
- [x] A file that does not parse is reported (exit 1) and left untouched
      (`try_format`); lexical errors count, and are now folded into every parse's
      diagnostics (`Parse::diagnostics`) — `viso check` used to accept an invalid
      escape silently.
- [x] `Viso.toml` normalization (§16, `viso_project::format_manifest`): spacing
      around `=`, trailing whitespace, trailing-comment spacing, blank runs, final
      newline; comments, order, arrays, inline tables and indentation kept; the
      result is checked to hold the same document before it is written.

### C1.5 — `viso check` (§17)

- [x] `check <target>`: target capability checks (a GPU-only node on `web-dom`, a
      platform-gated API on a target that lacks it) on top of the fast default (§17.1).
      (The target's configuration and availability, and the input its devices have;
      `web-dom` computes its page and reports the C4.4 findings; `web-hybrid` computes
      its split page the same way, and a view without an `Island` is
      `WEB_HYBRID_NO_ISLAND`.)
- [x] `--watch`: reuses the dev session watcher; re-checks on change, prints a
      per-batch summary, emits one `summary` per batch under `--json`, Ctrl-C exits
      130. (Per §17.3: `progress{phase:"idle"}` ends each round; one `summary`, at
      the end, with the last round's counts.)
- [x] Optional Rust side: `--cargo` runs `cargo check` for the project (default off,
      §17.2 keeps check fast).
- [x] `--a11y strict` and `--i18n strict` (DSL §U8.2, §U10) make those findings
      errors.
- [ ] A platform-gated native on a target that lacks it: natives declare no target
      availability yet (every capability they require exists on every available
      target). Blocked until the native schema gains per-target availability; the
      check lands with the first native that is missing somewhere.
- [x] A schema-enum property value names a variant of its enum: the widget enums
      (`Sizing`, `Track`, `Axis`, `Justify`, `Align`, `GridAlign`, `Alignment2D`,
      `FontWeight`, `TextOverflow`, `TextAlign`, `LiveRegion`, `Role`, …) and the
      records `Border`, `AdaptiveColumns` are prelude declarations, so a binding is
      typed against them (`E2001` unknown variant with the nearest, `E2103` another
      enum's or another type's value; a length still stands for `Sizing::fixed`); a
      `Head::x` whose head names no type is `E2001` with the nearest type.
- [x] `check` lowers the view each file mounts as its `view!` does, so what does not
      mount (`E3711`, `E9106`) is reported before a build.
- [x] A binding no path applies is `E3711` naming it, never dropped at mount; the
      constant `justify`, `align`, qualified `Axis`, `Sizing` variants and `const`
      values lay out, and `visible`, `focusable` and text `color`/`font_size`/
      `soft_wrap` reach the node (DSL D2.5 lists the rest).
- [x] `Offset` properties are typed as the prelude record (`Offset::zero()` beside
      it), so a value of another type is `E2103`; a `transition.*` value of another
      type than `Transition` is `E3703` (DSL §U6.1).

### C1.6 — `viso build` (§14)

- [x] `build [<target>]` with profiles `dev`, `release`, `shipping` (`--release` is
      `--profile release`; `--release` with another `--profile` is a usage error);
      unknown profile C0009; `hot_reload` refused in release/shipping (C0008).
- [x] One Build Service (`tools/tooling/src/build/`) shared by `run` and `build`
      (`package` C5 and `test` C1.9 build through it): resolves profile and target,
      computes the `BuildId`, runs cargo with the profile's flags (`shipping` is
      Viso's own Cargo profile inheriting `release`), packs for the target (`.app`,
      APK, web bundle) through `viso-package`, writes into
      `target/viso/build/<build-id>/out/` and swaps it into `dist/<platform>/` whole
      with its `artifact.json`; `run` keeps its dev features on top.
- [x] Build lock `build-<target>` (§44: non-blocking for `build`; a held lock is
      exit 3 with holder pid, age and path; stale by age only, taken over with a
      warning); `run` waits for it before each of its builds.
- [x] Artifact summary (§14.4): kind, path, size, profile, target, build id, hash;
      stage timings under `--verbose`; `artifact` event (§36.2) for `build` and for
      `run`'s launch build; `summary.artifact_count`.
- [x] Web optimization policy `[profile.<name>.web]` (§14.3): `wasm-opt` level, strip,
      brotli/gzip precompression, source maps — honoured by `build web-*`; a missing
      `wasm-opt` is a warning in dev/release, `ENV_TOOL` in shipping.
- [x] Rust diagnostics from cargo's JSON become `diagnostic` events with spans and
      related labels under `--json`, in `build` and `run`.
- [x] `run`/`serve` refusing a non-dev `--profile` names
      `viso build --profile <name>` as its next command (`failure_next`).
- [x] Ctrl-C kills cargo, `wasm-bindgen` and `wasm-opt` with their process trees,
      removes the staging output, leaves `dist/` as it was, exits 130.

### C1.7 — `viso run` remaining options (§13.7)

- [x] `--open` (web: open the browser, under `--json` too; elsewhere, or with
      `--browser none`, a usage error), `--env KEY=VALUE` (repeatable, a portable
      name outside `VISO_DEV_*`, passed to the app through its launch variables on
      every target; a value whose name looks secret is shown as `<hidden>`, §49),
      `--cwd <dir>` (host only; taken from the invocation directory; default the
      project root).
- [x] No `run --release` (§13.7): a non-dev `--profile` is a usage error naming
      `viso build --profile <name>`; release verification is `viso build`/`package`.
- [x] `--inspect` and `--profile-frame` land with the inspector channel and frame
      profiler (C6).

### C1.8 — Headless target (§3.4)

- [x] `headless` builds and runs on the headless platform backend and headless GPU
      (`viso-platform::backend::headless`, `viso-gpu::headless`); no window, no
      device; usable only by test/tooling commands (`run headless` is a usage error
      pointing at `test`).
- [x] Deterministic clock, fixed DPI and surface size from `[test]` in `Viso.toml`
      (default 1280×720 @1x); fonts from the project plus the bundled fallback only.
      (The framework bundles no fallback face, so a headless build reads the
      project's fonts only; `[test.surface]` reaches the build through
      `VISO_HEADLESS_SURFACE`, which `viso test` passes — C1.9.)

### C1.9 — `viso test` (§22)

- [x] Domains `unit` (cargo test of the project), `ui` (headless UI scenarios under
      `tests/ui/`), `game` (existing), `web` (C4), `all` (every available domain;
      unavailable ones are `skip` with the reason).
- [x] Headless UI tests (§22.2): load the app headless, drive input from a scenario
      (`*.vs` test blocks or tape files — decided with `Viso_DSL_1.0.md`), assert on
      the inspector tree and on snapshots (C2.5). (Tape files `tests/ui/*.tape`:
      click/hover/scroll/type/key/wait/expect on the semantics tree; snapshots are
      C2.6.)
- [x] `.vs` native nodes carry their semantics, which scenarios select by: the role
      `semantics.role` names (a constant `Role::<variant>`) or the widget's, the name
      `semantics.label` gives or the text a `Text`/`Button` shows, re-projected when
      a state they read changes; roles `heading`, `list`, `list_item` added.
- [x] The rest of `.vs` semantics: `semantics.hint`, `value`, `hidden` and
      `heading_level` re-projected as the states they read change, `live` a constant
      `LiveRegion::…`; a hidden node's subtree leaves the semantics tree; the direct
      children of `Tabs` are tabs and of `RadioGroup` radios (through regions too),
      `NavigationStack` is navigation; AccessKit and the web publish them.
- [x] The default roles of `Modal`, `Sheet` (`dialog`) and `Toast` (`status`,
      polite) once they are `.vs` widgets.
- [x] Filters (§22.4): `--filter <pattern>`, `--exact`, `--fail-fast`, `--nocapture`,
      `--update-snapshots`, `--target <t>`, `--device <profile>` (C3). (`--target`
      takes `headless`, and `ios`/`android` for `ui` with `--device`.)
- [x] One `test` event per case (`name`, `domain`, `status`, `duration_ms`, failure
      with location), a tally in `summary`; exit 1 on a failed assertion, 5 on a
      crashed runner (§7).
- [x] Tests run in parallel up to `--jobs`, each case isolated (own runtime, own
      temp dir); output of a passing case is captured. (UI cases; game scenarios run
      one after another in one process, each on its own world.)

### C1.10 — `viso clean` (§45.1)

- [x] `clean` removes `target/viso/` (build, cache, dev, generated, traces, locks),
      never `dist/` or `~/.viso/`; refuses with `ENV_LOCK_HELD` (exit 3), listing
      each holder, while a lock that is not stale is held; JSON
      `result{removed: [path], freed_bytes}`; human output prints the freed size
      (`human::size`). `Layout::cleanable` is `target/viso/` whole.
- [x] Locks refreshed by their holder every 20 s (a thread per guard, file mtime);
      stale after 90 s unrefreshed; `acquire` takes over a stale lock and the
      command warns (`Context::lock`); `LockGuard::held` lists the live ones.
- [x] Never deletes outside the layout's `cleanable()` set: links are removed, not
      followed (`tests/clean.rs`, with a symlink pointing outside).

---

## C2 — Compiler tooling (§18–§21, §23, §24.3, §58, §71)

Goal: AI and IDE tools get structured, stable interfaces to the compiler: schema,
explanations, IR dumps, the LSP, headless snapshots and tree queries.

### C2.1 — What exists

- [x] `viso schema <query>`: typed schema of a widget, native library, function or
      type, human and JSON (tests in `tests/schema.rs`).
- [x] `viso-lsp` binary in `crates/lsp` (formatter, index, actions, source map).

### C2.2 — `viso schema` remaining (§18)

- [x] All query forms of §18.1 (`Widget`, `Widget.prop`, `module::Item`, `--list
      widgets|components|natives|types`), project components included with their
      source origin (`file:line`, §18.3), framework items marked `framework`.
      (`schema::Catalog` + `package_components`: each input's `invalidates` from
      the view's binding edges, control values and region decisions.)
- [x] JSON schema output stable and versioned (`schema_version`), golden-tested
      (§58). (`tests/golden/schema.json.txt`; `viso --version` reports
      `schema object`.)
- [x] Project records and enums as schema symbols (`kind: record|enum`, fields and
      variants), so `--list types` covers the package's own types. (`package_schemas`;
      fields and variants are members, `Point.x`, `Status.ready`.)
- [x] A component's slot cardinality (`Slot`/`OptionalSlot`/`SlotList`) and
      `@default` in the schema object instead of a bare name. (`slots[{name, type,
      default}]`, a widget's from its native schema too.)

### C2.3 — `viso explain` (§19)

- [x] A diagnostics registry in `viso-dsl`: every code (`E####`, `Lex####`,
      `Parse####`) with title, long explanation, a failing example and the fix; one
      table, generated docs read from it. (`viso_dsl::diag::codes`, 127 codes, the
      lexer and parser use `E1xxx`; `docs/diagnostics.md` generated by its test.)
- [x] A test enumerates every code the compiler can emit and fails if one is missing
      from the registry (and vice versa); same for CLI codes (`C####`, `CLI_*`,
      `ENV_*`, `BUILD_*`, `TEST_*`, `DEVICE_*`, …) in a CLI-side registry.
      (`tests/diagnostic_registry.rs`: the codes `crates/*/src` raise, the
      specification's table, each `.vs` example compiled for its code and
      severity; reserved E2702, E3002, E3707, E3709. CLI: `error::ALL` each with
      explanation and fix and each raised; `ConfigCode` explanations.)
- [x] `viso explain <code>` prints it (no project needed), unknown code exit 1 (the
      spec's) with nearest codes; `--json` gives
      `result{code,title,explanation,example,fix,…}`; `--list` lists all codes.
      (`EXPLAIN_UNKNOWN`; a Rust `E0XXX` points at `rustc --explain`.)
- [x] `viso check` and the event stream link each diagnostic to its explanation
      (`= help: viso explain E3101` on errors whose fix the message does not
      already say). (An error with no fix of its own, a config error with no note,
      gets the fix `run \`viso explain <code>\``: no edits, as §53's next commands.)

### C2.4 — `viso dump` (§20)

- [x] Stable textual and JSON dumps from `viso-dsl` for `ast`, `hir`, `ui-ir`,
      `reactive-ir` (binding IR + dirty map), `behavior-ir` (existing `dump()`
      generalized), `shader-ir`, `system-ir`, `module-graph`; one `Dump` trait, no
      `Debug` formatting as output (`crates/dsl/src/dump/`, the shader printer in
      `viso-shader`; `Compiled::invalidations` keeps what each source invalidates).
- [x] `viso dump <kind> [<file>|<symbol>] [--symbol <path>] [--out <file>]`; source
      spans included; dumping an invalid file prints diagnostics and dumps what lowered.
- [x] Golden tests per kind on a fixture project; the dump format is documented as
      unstable-for-humans but its JSON keys versioned (`dump_version`, in `viso --version`).
- [x] The HIR records module-level `fn`/`action`/`task` nodes (they were never filled).
- [x] `ui-ir`/`reactive-ir`/`behavior-ir` of a file importing a sibling module's
      declaration: the one-file compile reads its imports from the loaded package.

### C2.5 — `viso lsp` (§21)

- [x] `viso lsp` execs `viso-lsp` (found next to `viso`, then on `PATH`), forwarding
      stdio and args; missing binary is exit 3 with the install hint (`ENV_LSP`); no
      `--json` (`CLI_USAGE`).
- [x] `viso-lsp` uses the shared formatter (C1.4) and the diagnostics registry for
      hover on codes.
- [x] `viso-lsp` exits 0 after `shutdown`+`exit`, 1 otherwise (it always exited 0).
- [x] `viso doctor` reports whether a `viso-lsp` of the CLI's version is installed.
      (The `lsp` check asks `viso-lsp --version` of the server `viso lsp` would run.)

### C2.6 — `viso snapshot` headless (§23)

- [x] `snapshot capture <scenario>`: render headless at the test surface, write
      `tests/snapshots/<name>.<target>.png` plus a sidecar (tree hash, build id,
      renderer, surface, size, tolerance); a scenario without snapshot steps captured
      by name takes its end; `--output <dir>` writes elsewhere.
- [x] `snapshot capture <component>`: a component alone, mounted by a harness the
      headless build carries, at the test surface; its golden records the component,
      so `compare`/`update` take it again with the scenarios.
- [x] The software raster's CPU shader programs compile once into a flat register
      program run on a quad's four lanes at once, allocating nothing per instance or
      pixel: a full-screen custom shader at 1280×720 (release, `cpu_shader` bench)
      went from 1.15–1.30 s to 160–180 ms a frame.
- [x] `snapshot compare`: perceptual (YIQ over white) + exact compare with a
      per-snapshot tolerance from the sidecar, diff, image taken and tree into
      `target/viso/snapshots/`, exit 1 on mismatch, one `test` event a golden, a
      golden no scenario takes fails.
- [x] `snapshot update [--filter]`: rewrites only missing or mismatched snapshots,
      keeping their tolerance, one `artifact` each.
- [x] `expect snapshot "<name>"` steps in UI scenarios and `test ui
      --update-snapshots`, through the same compare.
- [x] `--target headless` default; other targets exit 3 (`ENV_SNAPSHOT_TARGET`) until
      C3/C4.
- [x] The app template packages a font from `assets/fonts/` (`use_packaged_fonts`),
      so the headless snapshots of a new app show its text: headless resolves only
      the project's fonts.
- [x] `viso new --framework` relates the framework's path through links (`/tmp` is
      `/private/tmp` on macOS).

### C2.7 — `viso inspect query` (§24.3)

- [x] `inspect query <selector>` against a headless instance (or a running session in
      C6): selector syntax (type, `#id`, `.class`, `[prop=value]`, descendant), JSON
      result of matched nodes with layout rect, props and source span. (Type, `*`,
      `#name`, `.Style`, `[attr]`/`[attr=value]` over role, label, hint, value,
      checked, selected, expanded, focused, focusable, shown, kind; descendant, `>`
      and `,`. The running `viso run` session of any target, through its control
      endpoint and the `Inspect` dev message (protocol 15), or `--target headless`
      after an optional `--scenario`; `INSPECT_SELECTOR` exit 2, no session 3, no
      match 0. A component's name selects its root node, the view's own included;
      `tests/inspect_session.rs` queries a live session before and after an edit.)
- [x] Shared Inspector Service type used by tests (C1.9), Studio and the CLI.
      (`viso_view::inspect`: the runtime's census, the tool's source table from
      `viso_dsl::inspect`, their join and the selector; UI scenarios select by it as
      `click (Column > #save)`; Studio is to use it once it exists.)

---

## C3 — Mobile development environment (§11, §12, §13.2, §72)

Goal: Android and iOS simulator/emulator development without the user ever typing
`sdkmanager`, `avdmanager` or `simctl`; the Toolchain/Emulator service, the Simulator
service and the platform runtime backend stay separate.

### C3.1 — What exists

- [x] `viso android emulator` and `viso ios simulator`
      `list|create|start|stop|delete`; profiles under
      `~/.viso/<platform>/{emulators,simulators}/<id>.toml` (`$VISO_HOME`); AVD written
      without `avdmanager`; `--api`, `--preset`, `start --no-window`.
- [x] `viso run ios|android`: device selection (flag, local, user, sole profile),
      TTY prompt with non-TTY/CI/JSON fallbacks, boot during build, dev `.app`/APK,
      launch vars, exit records (iOS `log stream`), warm restart.
- [x] Fake Android backend for tests (`deploy/android_fake.rs`, `tests/device.rs`).

### C3.2 — `viso android list|use|doctor` (§11.1–§11.3)

- [x] `android list`: installed and available API levels with their component set
      (platform, build-tools, platform-tools, emulator, system image per host ABI,
      NDK), marking the active one; `--offline` lists installed only.
- [x] `android use <api>`: download the pinned component set into
      `~/.viso/android/sdk/` — each archive's size and SHA-1 (the digest Google's
      repository manifest publishes) verified against the pinned table, extracted to a temp dir, moved into place atomically, only the files a
      build needs kept (makepad's selective expansion); resumable; progress events;
      accepts the license by an explicit `--accept-licenses` or a TTY prompt, never
      silently; `--offline` fails with exit 3 when anything is missing. (`curl` over
      HTTPS only, resumed from `.part`; the CLI's own zip reader with ZIP64, CRC and
      zip-slip/link checks; staging dir + one rename; `.viso-component` marker.)
- [x] Active SDK recorded in `~/.viso/android/config.toml`; resolution order
      `ANDROID_HOME`/`ANDROID_SDK_ROOT` > active Viso SDK > default locations,
      reported with origin.
- [x] `android doctor`: SDK, NDK, build-tools, platform-tools, emulator binary,
      system image for the host ABI, hardware acceleration (HVF/KVM/WHPX), Java only
      where a step needs it, rustup targets; `doctor_check` events; each failure's
      fix is a `viso` command. (Checks `android-components`, `android-accel`
      (`emulator -accel-check`), `java` added to the Android plan.)
- [x] Unblocks the Android API 35/36 image install noted in `todo_hotreload.md`.
      (Real `viso android use 36` installed and verified the set, resuming the
      system image across two dropped connections; patch and warm restart then
      verified on that emulator.)

### C3.3 — `viso android adb` and logs (§11.5, §11.6)

- [x] `android adb [--device <profile>] -- <args>`: resolves the profile to the
      running emulator's serial, runs the SDK's `adb -s <serial>` with the args
      verbatim, exit code passed through; not running is exit 3 with the start
      command.
- [x] ~~`android logs [--device]`~~ — superseded: §11.6 keeps no separate logs
      grammar; `viso run android` gathers the app's log and `viso android adb
      --device <p> logcat` is the low-level path.

### C3.4 — `viso ios list|use|doctor` (§12.1–§12.3)

- [x] `ios list`: installed simulator runtimes and device types (`simctl list -j`),
      the active one marked; downloadable runtimes when Xcode can report them.
      (Xcode reports only the runtime its SDK pairs with, `simctl runtime match
      list`; that one is listed `available`.)
- [x] `ios use <runtime>`: sets the default runtime for new profiles; a runtime not
      installed is installed through `xcodebuild -downloadPlatform iOS` only with
      explicit consent (flag or TTY prompt); never touches Xcode itself.
- [x] `ios doctor`: Xcode and command line tools selection, license accepted,
      simulator runtime, `aarch64-apple-ios-sim` target, disk space for a runtime;
      on a non-macOS host every `ios` command is exit 3 with one clear diagnostic.
      (`ENV_IOS_HOST`; checks `xcode-license`, `ios-disk` added.)
- [x] Simulator profiles are created on the default runtime when it is installed,
      else the newest.

### C3.5 — Mobile `build`, `test`, `logs`

- [x] `build ios|android` through the Build Service (C1.6): simulator `.app` (ad-hoc)
      / emulator APK (dev APK in dev, release APK otherwise); device signing is C5.
- [x] `test --target ios|android --device <profile>`: UI tests on the simulator or
      emulator through the same runner protocol as headless (C1.9).
      (Each case is a fresh `viso run <platform> --device`; the scenario goes through
      its control endpoint to the app, which plays it on its first window — input
      posted to the scheduler as the platform's, expectations read from its
      semantics — and reports each line back. Snapshot steps report skipped, component
      cases are headless only. Verified on the API 36 emulator: a passing tape, a
      failing step reported at its line, a snapshot skipped.)
- [x] ~~`ios logs [--device]`~~ — superseded: §11.6/§12.6 keep no separate logs
      grammar; `viso run ios` streams the app's console and its exit record.
- [x] Fake iOS backend (simctl replaced by a scripted fake) beside the Android one,
      covering boot failure, disconnect mid-session and SDK missing (§61).
      (`deploy/ios_fake.rs`: Xcode's tools resolved through a test-only directory;
      boot failure, simulator gone, abort, shutdown mid-session, no `xcrun`. `viso ios
      list/use/doctor` against fakes on `PATH` in `tests/ios.rs`.)

---

## C4 — Web (§3.3, §14.3, §15, §31–§33, §73)

Goal: `web-gpu`, `web-dom` and `web-hybrid` build, run and serve, and a project that
uses something a web target cannot do is told so at check time with the node and the
alternative.

### C4.1 — What exists

- [x] `viso run web-gpu`: wasm build, `wasm-bindgen` bundle, local server, dev channel
      over WebSocket, page log channel, reload, browser choice.
- [x] `viso serve [web-gpu]`: `--host`, `--port` (exact when given), `--lan` with
      Host/Origin checks (421 for an unserved Host), `--open`, `--no-hot-reload`.
- [x] Several web lanes in one session (ADR 0046, §15.4): `viso serve web-gpu web-dom
      web-hybrid` (any subset, each once) and `viso run` with several web targets share
      one watcher, source graph, control endpoint and page server on one port.
  - [x] One lane keeps `/`; several are each under `/<target>/`, `/` lists them, a
        prefix without its slash is a `308` keeping the query.
  - [x] Dev and page channels beside the page (`<lane>/__viso/dev`, `<lane>/__viso/page`):
        the runtime and the page script resolve them from the page's URL, so each socket
        belongs to its lane; a lane's new build reloads its pages alone.
  - [x] COOP/COEP per lane, only where the bundle's module imports or defines a shared
        memory (`viso_package::web::shares_memory`).
  - [x] One human line and one `server` event (`target` added; `url` with the lane's
        path) per lane; the LAN warning once; `--open` opens every lane's page.
  - [x] End to end with a `web-hybrid` lane: `viso serve web-dom web-hybrid` on one project
        serves both lanes; in headless Chrome the hybrid lane's page pushes into its island
        and runs the island's event, the `web-dom` lane shows the island's component inline.

### C4.2 — web-gpu remaining

- [x] `build web-gpu` through the Build Service with the optimization policy (C1.6):
      a static directory with precompressed files.
- [x] Hashed asset names in the web bundle (`app.<hash>.js`, `app_bg.<hash>.wasm`,
      the page naming them) so a server can cache them forever. (16 hex digits of
      each file's SHA-256 after `wasm-opt`; the page passes the module to `init`, so
      the generated glue is not rewritten; `index.html` keeps its name.)
- [x] `[profile.<name>.web] split`: a startup/lazy module split. (`auto|on|off`; a
      training run in a headless browser profiles the module `wasm-split`
      instrumented, the functions it called stay in the primary part, the rest load
      in the background, a call before then loads them on the spot; `auto` splits a
      shipping build without debug info when `wasm-split` and a browser are found.
      The map's shipping startup download: 1.86 → 1.19 MiB brotli.)
- [ ] `[profile.<name>.web] threads`: a shared-memory threaded module (`auto`
      resolves to off until then); with threads on, `serve` sends COOP `same-origin`
      and COEP `require-corp`. (The serve half is in: a lane whose module shares its
      memory gets both headers.)
- [x] `serve` security headers of §15.3 (CSP, `nosniff`, correct MIME for `.wasm`),
      served precompressed when the client accepts it. (The page is the bundle's own
      `index.html` with the launch scripts ahead of `</head>`; a per-response nonce on
      each script, CSP on scripts only; CORP `same-origin`; `.br` before `.gz` by
      `Accept-Encoding` with `Vary`. COOP/COEP move to the threads item.)
- Not planned: `serve --https` (a loopback page is a secure context; another device
  reaches it as loopback through port forwarding).
- [x] Page-side `print`/`eprintln` log API: `viso::println!` and its family (`todo_hotreload.md`).
- [x] `test web --target web-gpu`: headless Chrome/WebDriver runner when a browser is
      found, `skip` otherwise. (`test ui --target web-gpu`: the device runner with a
      browser venue — `viso run web-gpu --browser none`, the served page opened in
      headless Chromium (`VISO_CHROME`, else Chrome/Chromium/Edge), the scenario through
      the control endpoint; no WebDriver needed.)

### C4.3 — web-dom and web-hybrid

- [x] `web-dom`: the page runtime is `viso-dom` (the view's behavior on the VM in the
      app's wasm, called by the export's DOM runtime); `build`, `package` and
      `check web-dom` are done.
  - [x] Build target and bundle: `viso/web-dom` feature, `view!` offers its module,
        `viso_dsl::export::dom` lowers the page, `dist/web` gets the page, styles,
        script, wasm and, in dev, the view's module.
    - Built: `Page` (`crates/dom`), `visoDom`/`V` on wasm, `Engine::Vm` in the export
      script, `__view_mount!`, `WebPage` staging; initial texts prerendered.
    - Verified: `viso new` app → `viso check web-dom` clean, `viso build web-dom` dev
      page clicked in headless Chrome (0 → 1 → 3 clicks); release wasm 912 KiB.
  - [x] `run`/`serve web-dom`: a dev session lane; a `.vs`, catalog or asset edit lowers
        the page again and, when it changed, writes it into the served bundle and reloads
        the lane's pages; a Rust edit rebuilds and serves the new bundle.
    - Built: `Deploy::Browser.page` (`PageSource`), `Deploy::repage`, `write_dom_page`
      replacing the page before it; the lane checks its sources at start as `build` does.
    - Verified: `viso run web-dom --browser none` on a `viso new` app: an added `Text`
      showed after reload with no Rust build, a broken edit kept the page and reported
      its error, a `main.rs` edit rebuilt and restarted; `write_dom_page` test.
  - [x] Package natives in the page lowering: the export links the natives the view
        compiled against (a package's are schema stubs); a value calling one is left to the
        page's VM rather than prerendered. Verified: `export_dom` test with a schema-read
        library. (An initial state calling one still does not lower.)
- [x] `web-hybrid` (§32, ADR 0059): a DOM page with one `Island` the renderer draws; the
      bundle carries both runtimes (`viso/web-hybrid`) and the split page
      (`export::hybrid`).
  - Built: `Island` in the IR (`ViewBehavior::islands`), the hybrid lowering, the facade's
    island mount (`viso_dom::shared`, `viso_dom::island`, `ViewHost::push_input` /
    `set_event_sink`), the target in `build`/`check`/`run`/`serve` (`WEB_HYBRID_NO_ISLAND`).
  - Verified: `dsl_island` test; `viso build web-hybrid` and `viso run web-hybrid` in
    headless Chrome with WebGPU (page → island inputs, island → page events, a `.vs` edit
    rebuilt the island).

### C4.4 — Web capability diagnostics (§33)

- [ ] Capability table per target (GPU-only nodes, custom shaders, threads, file
      system, native libraries) in the compiler, consumed by `check <web-target>` and
      `build`; each diagnostic names the node, the target and the alternative
      (`WEB_*` codes registered in C2.3). `web-dom`'s rows are done:
      `WEB_GPU_ONLY_NODE`, `WEB_DOM_UNSUPPORTED`, `WEB_DOM_NO_VIEW`, from the page
      lowering's findings, in `check web-dom` and `build web-dom`; `web-hybrid`'s are the
      same outside the island (a second island or one in a region is
      `WEB_DOM_UNSUPPORTED`) plus `WEB_HYBRID_NO_ISLAND`. On `web-gpu` every node, shader
      and native the compiler knows is available, so the rest of the table is the
      platform-gated natives' (the item above). The
      §33 classification already runs where a target lacks something today — the
      export lowering's findings (`viso export html|solid`), each at its span with
      the node, the capability and the alternative.

---

## C5 — Delivery and export (§2, §27–§30, §74)

Goal: `viso package` produces a signed, described, reproducible deliverable per
target; `viso export html|solid` produces source a web project owns, and refuses with
a precise diagnostic what it cannot express.

### C5.1 — What exists

- [x] `viso-package`: `.app` wrapping and signing (ad-hoc, identity, provisioning
      profile), APK with the Java shell (debug keystore), web glue and page.
- [x] `cargo xtask bundle` for framework-internal packaging (stays a dev tool; the
      user surface is `viso package`).

### C5.2 — `viso package` (§27)

- [x] `package [<target>]`, default profile `shipping`; `--profile dev` is exit 2
      (§2.2); host targets: macOS `.app` + `.dmg`, Windows zip, Linux tar.gz; iOS
      `.ipa` (device triple, actool asset catalog); Android `.apk` and `.aab`, one
      library per `[package.android] abis`; web `site/` + zip. Zip and tar are
      written by the packager itself, the same bytes for the same files.
- [x] Package metadata (§27.1) from `[package]` in `Viso.toml`: id, display name,
      version, build number, icon (square PNG ≥ 1024 px, scaled in linear light),
      permissions, orientation; validated (C diagnostics at their line) before
      building.
- [x] Signing (§27.2): `--signing auto|required|off`; identities from
      `[package.<platform>]` or the keychain, keystore passwords from
      `VISO_ANDROID_KEYSTORE_PASSWORD`/`VISO_ANDROID_KEY_PASSWORD` read by the tools
      (`env:`), never on the command line or in logs; `required` with no identity is
      exit 6; `auto` short of a real one warns `PACKAGE_UNSIGNED`.
- [x] `--dry-run` (§27.3): prints the plan (metadata, signing identity chosen,
      steps, output paths) and validates metadata and signing without building.
- [x] Package lock `package-<target>`; output `dist/<platform>/` with
      `artifact.json` (§27.4: files with SHA-256, size, target, profile, build id,
      build number, signing object, toolchain); `artifact` events; exit 6
      (`PACKAGE_SIGNING`, `PACKAGE_ARCHIVE`).
- [x] Windows: the icon (16–256 px) and `VS_VERSIONINFO` linked into the executable —
      a `.res` for MSVC (byte-identical to `llvm-rc`'s), a COFF `.rsrc` object for
      GNU, named by content so a change relinks, passed by
      `cargo rustc --bins -- -Clink-arg=…` (`--bins` also lets a lib + bin package
      take rustc arguments, Android's too); the resource tree checked through
      `lld-link` and MinGW `ld.lld`. An MSIX beside the zip when there is an icon:
      written by the packager (64 KiB blocks deflated alone, SHA-256 block map,
      content types), full-trust manifest with capabilities from the permissions,
      `[package.windows] publisher` (required to sign it; unsigned ones carry the
      unsigned OID); unpacked and schema-validated by Microsoft's MSIX SDK
      (`makemsix`). Version numbers ≤ 65535 and the package name checked first.
- [ ] Windows on a Windows host: `signtool` signing of the executable and the MSIX,
      and `Add-AppxPackage` of the MSIX, are written but unverified (no Windows
      machine here).
- [ ] Verify on real credentials: a Developer ID signature notarized
      (`notarytool`, stapled), an `.ipa` signed with a distribution profile, an
      `.aab` accepted by `bundletool validate`/Play; this machine has no identities.

### C5.3 — `viso export html` (§28, §29, §31)

- [x] Capability analysis (§28.2): the exporter reads the UI IR, the behavior table
      and the Behavior IR (never the `.vs` text); every entry is run by the
      interpreter against the initial states, inputs' defaults, the default theme and
      the source locale, and what it reads (`Vm::reads`) classifies it constant,
      theme (a CSS variable) or changing, before any file is written. Each finding
      is a diagnostic at its span: `EXPORT_HTML_DYNAMIC_REQUIRED`,
      `EXPORT_HTML_GPU_ONLY_NODE`, `EXPORT_UNSUPPORTED`; exit 1.
- [x] Layout lowering to HTML + CSS (§29.2): Flex/Grid/Scroll as flexbox, grid and
      overflow containers with the runtime's sizing (leaf default 0, containers
      Fill, unset side Fit, cross Fill natural unless stretched), widgets as their
      HTML elements and roles (button, checkbox switch, range, input, img, tabs and
      radio groups with `aria-selected`/`aria-checked`), semantics as ARIA, styles'
      `When` arms as `:hover`/`:active`/`:focus-visible` rules, transitions with a
      reduced-motion rule, theme tokens as `:root` custom properties, shared classes
      deduplicated; assets copied under `assets/`; output deterministic.
- [x] The page script (§29): every reachable Behavior IR function translated to
      JavaScript with the interpreter's value model (BigInt integers range-checked
      or wrapped, `F32` rounded, aggregates `{t, f}`, lists copy on write); each
      handler one transaction that refreshes only the updaters whose state it
      changed; `if`/`match`/keyed `for` regions mounted from `<template>`s (with
      `preserve`), controls kept controlled after their own events, `emit` as a
      `CustomEvent` on `#viso-root`; `tr` formatted from the source locale's table
      with `Intl.PluralRules`/`NumberFormat`. Verified in headless Chrome: counter,
      regions, toggle, text input, assets.
- [x] `--static` (§29.1): no script; what changes is `EXPORT_HTML_DYNAMIC_REQUIRED`
      at its span, a shader node `EXPORT_HTML_GPU_ONLY_NODE` with the `web-gpu` /
      `web-hybrid` alternative.
- [x] `[COMPONENT]` (`Name` or `module::Name`) exports one component alone, inputs
      at their defaults; the default is the one `src/app.vs` exports
      (`EXPORT_NO_VIEW` when there is none).
- [x] Generated-file safety (§51): the export dir is replaced whole through a staging
      directory beside it; `.viso-generated.json` records each file's SHA-256, and a
      directory holding a file the export did not write, or one edited since, is
      `EXPORT_CONFLICT` (exit 6) unless `--force`; the project, an ancestor of it,
      `src/` and `assets/` are refused as the export dir. `artifact`
      `kind:"export-dir"`, `profile: null`.
- [x] The page runs what the view starts: `start`ed tasks as `async` functions whose
      task natives (`time::sleep`) are awaited promises, launched after the commit,
      each slot under its policy (`keep_latest` cancels and runs `cancelled`,
      `drop_new`, `queue`, `parallel(n)`), the `success`/`error` handler a new
      transaction; `effect`s gated by their `when` value and run policy after the
      commit that mounts them and each that changes it, settling in rounds, with
      their cleanups; `resource`s through `loading`/`reloading`/`ready`/`error` with
      `debounce`, `keep_latest` and `cache_for`/`cache_errors`; a component a region
      mounts keeps its states per mount, runs its effects there and owns the tasks it
      starts, all ended without a handler when the content goes. `--static` refuses
      an effect as `Need::Dynamic`. `tests/export_html.rs` runs such a page in
      headless Chrome. Fixed on the way: a region placed at the top of region content
      left the items it mounted in the template fragment; and natively, a static
      instance whose own view has a region ran its effects twice.
- [x] The Solid export runs the same, on one engine with the page (`work.js`): the
      store starts tasks after its commit and runs effects and resources by the same
      rules; region content mounting a component is an `st.mount`, whose signals keep
      that component's states, whose effects run there and which owns its tasks, all
      ended with its Solid owner; a computed reading such states is no shared memo.
      Verified: `tsc` clean, `vite build`, and the same page in headless Chrome.
- [x] `env` fields beyond theme and locale come from the browser (`env.js`, shared by
      the page and the Solid store): the window, safe area (the `env(safe-area-inset-*)`
      probe the web backend uses), keyboard inset (the visual viewport), display
      features (viewport segments), input capabilities (media queries, touch points,
      gamepads), text scale (`font-size: medium`), reduced motion, orientation and
      layout direction, each followed through its events. `env.constraints` and
      `env.size_class` are measured at their reader's root as the native layout
      resolves them, from what the elements say of their layout (`data-root`,
      `data-fit` for an extent its content decides, `data-scope` for an
      `AdaptiveScope`), and followed with a `ResizeObserver`; in a region each mount
      keeps its own. Verified in headless Chrome for both exports at 1000×700.
      A game's systems stay the Viso runtime's: `viso build web-gpu`.
- [x] Locales beyond the source one: `app.js` (and the Solid module) ships every
      locale's table, untranslated entries already pointing at their CLDR fallback.
      At load, `settle` picks the locale nearest `navigator.languages`: the tag
      itself, then shorter forms, then its `maximize()` script, so `zh-TW` reaches
      `zh-Hant`; the source when nothing matches. It sets `<html lang>`, `env.locale`
      and the number language. Text that reads the locale is written by the script
      rather than into the HTML; a `--static` page stays in the source locale. Plural
      rules follow the language the pattern is written in, as the native runtime
      does. `tests/export_html.rs` runs the choice under node for four browser
      languages; checked in Chrome with `--lang=de-DE`/`en-US`.

### C5.4 — `viso export solid` (§30)

- [x] SolidJS component source from the same lowering as C5.3 (§30.1): the lowered
      element tree written as JSX; states as `createSignal`s, computeds as
      `createMemo`s (read raw inside a transaction), inputs as typed props
      (`bigint | number`, `boolean`, `number`, `string`), events as `on…` callback
      props; each handler one transaction over the store, setting only the signals it
      changed in one `batch`; `if` as `Show`, `match` as `With` + `Switch`/`Match`,
      keyed `for` as `Each`, `preserve` arms as `Choose`; assets imported so Vite
      places them. The Behavior IR translated to TypeScript by the page translator's
      Solid dialect. The state example of §30.2 is a golden test.
- [x] SolidJS is never a Viso core dependency (§30.3): the export writes `package.json`
      (`solid-js`, `vite`, `vite-plugin-solid`, `typescript`), `tsconfig.json`,
      `vite.config.ts`, `index.html`, `src/` and its runtime; Viso runs no package
      manager (the human output prints the next step for `export.solid.package_manager`).
      Verified: `tsc` clean and `vite build` under Node 24, then counter, regions,
      controls, assets, props and event callbacks in headless Chrome.
- [x] Ownership (§30.4): generated header in every source file; the directory replaced
      whole only while unchanged since the last export (`EXPORT_CONFLICT`, exit 6,
      unless `--force`); `node_modules/`, `dist/` and lockfiles are kept as they are
      across exports; the artifact hash covers the generated files only.

---

## C6 — Full observability (§24–§26, §75)

Goal: a running app (host, simulator, emulator, browser) can be inspected, profiled
and traced from the CLI and from Studio over the same services.

- [x] `inspect tree|node|frame` (§24.4) over the dev runtime transport (`Inspect` ask
      and answer, protocol 17) and headless (`VISO_HEADLESS_DETAIL`): tree with
      source, node detail (resolved layout/style/content, boxes, transform chain,
      dirty classes, interaction states, event route, bound states with names, types,
      values and declarations from the source table, paint span), frame detail
      (counters, batches, GPU resources, caches, assets).
- [x] Inspector capabilities negotiated (§24.2): the runtime hello reports
      `tree`/`nodes`/`frame`/`gpu_time`/`events`; results carry `inspector` and
      `unavailable`; a missing one is `INSPECT_UNSUPPORTED`.
- [x] Inspector event stream (§24.5): input routes, focus changes and each direct
      dirty mark with its cause (input, state with name/value/declaration, computed,
      animation, task, reload, text, asset, window, layout), from a bounded journal
      the runtime keeps while asked (`viso-ui` `inspect` feature); `inspect events`
      over a session or a headless scenario; `inspect` JSON events.
- [x] Bare `viso inspect` / `inspect --attach <session>` / `inspect --run` (§24.1):
      attach, describe the app, follow its first window until interrupted; the session
      ID is published in the dev-session lock.
- [x] Per-node batch attribution: each paint-order entry records the stream index
      of the primitive that emitted it; a node's detail lists the batches its own
      draws land in, numbered as `inspect frame` numbers them.
- [x] `profile [<target>]` (§25): frame time, per-span CPU time, GPU time,
      layout/paint/reactive/text/upload counts, draw calls; `profile` events;
      `--frames <n>`, `--seconds <s>`; dev session and `headless [--scenario]`.
- [x] Trace output (§25.3): `--output trace.ende` (Ende binary trace), `--chrome`
      (Chrome trace JSON); traces under `target/viso/traces/` (newest 20 kept).
- [x] `run --profile-frame` HUD and per-frame `profile` events.
- [x] Allocation counts: `viso::alloc::Counting`, a counting allocator the app
      declares as its global allocator, reported per frame as `allocations` and
      `allocated_bytes`; `unavailable` without it.
- [x] GPU time on Vulkan, D3D12 and WebGPU from timestamp queries: a frame's
      recording start to its last pass's end (Vulkan, D3D12), each pass's
      `timestampWrites` where the browser offers `timestamp-query` (WebGPU).
      Checked: WebGPU in Chrome through `viso profile web-gpu`; the Vulkan
      queries validation-clean on MoltenVK offscreen; D3D12 compiles for
      Windows only.
- [ ] GPU time verified on a presenting Vulkan device (Android emulator's Vulkan
      fails to create an instance on this host) and on Windows D3D12.
- [x] HUD checked on screen: a macOS `run --profile-frame` window's presented
      frame carries the HUD's panel and its text run (`inspect frame`: 3 nodes and
      2 primitives beyond the tree, in their own quad and glyph batches); its
      layout is unit-tested.
- [ ] A pixel golden of the HUD: its text changes every half second and a
      headless run shapes no text, and this host grants no screen capture.
- [x] `run --inspect`: the session follows its first runtime's first window over its
      own control endpoint, the same journal `inspect --attach` follows.
- [x] Device profiles for mobile performance (simulator vs emulator caveats reported
      in the result).
- [x] Dropped: `viso studio`. There is no Studio; the editor extensions over the CLI's
      event stream take its place (`todo_next.md` N10).

---

## C7 — Cross-cutting contracts (grows with every phase)

### C7.1 — JSON protocol (§34–§37)

- [x] Event envelope and stream; `diagnostic`, `progress`, `device`, `server`, `dev`,
      `runtime`, `log`, `trace`, `test`, `result`, `artifact`, `summary`.
- [x] Every event type emitted (`doctor_check`, `profile` and `inspect` among them)
      in the `Viso_CLI.md` §36 table, each payload with an example.
- [x] Every command except `lsp`, `studio`, `completion` supports `--json`
      (`android adb` relays adb's lines as `log` events); `tests/json.rs` finds
      every command the help lists and holds each one's stream to the contract.
- [x] stdout carries only events under `--json` (child output becomes `log`),
      human output never writes events; every human write goes through one writer
      that drops what a gone reader would have read, so a closed pipe (`EPIPE`) ends
      the command as it would, never a panic (`tests/stdout.rs`).

### C7.2 — Reliability and security (§48–§51)

- [x] External processes (§48): one runner (`process.rs`) for every program the CLI
      starts. Args are passed as a list, never a shell string. `--verbose` echoes the
      command line with secrets hidden (also the argument after a secret's name);
      under `--json` the echo is a `log` event. Output and exit status are captured as
      data. Not installed, timed out and interrupted are distinct failures. A timeout
      or interrupt kills the process group or job. The env is inherited, since tools
      read their configuration from it. The duplicate per-command echoes and the
      ad-hoc capture loops (doctor, device, new, build) are removed. `viso-package`
      runs its tools through a runner the CLI installs (`run_tools_with`).
- [x] Secrets (§49): one rule (`secret.rs`) for what is never printed — a variable
      or `name=value` argument whose name holds `TOKEN`, `PASSWORD`, `SECRET`, `KEY`,
      `AUTH`… — applied to `--env` and the Cargo command line under `--verbose`. The
      CLI holds no secret value itself: signing passwords are read from the
      environment by `apksigner`/`jarsigner`, identities stay in the keychain.
- [x] Network downloads (§50): HTTPS only (redirects too), size and SHA-1 pinned
      from the SDK manifest, which publishes SHA-1 only (re-pinning as SHA-256 would
      mean fetching ~15 GB of archives for every host). Transfers stop at the pinned
      size and stalled attempts back off. `.part` + verify + rename, with resume.
      Manifests are capped at 16 MiB. `--offline` is one global flag, and the network
      layer refuses under it: the `android`/`ios` `list`/`use` subcommands had their
      own `--offline`, which hid the global flag, so `viso --offline android use`
      downloaded anyway. Proxy env is curl's.
- [x] Generated-file safety (§51): single files go through `atomic::write` (`fmt`,
      `dump --output`, game tapes, profile traces, snapshot goldens/diffs, toolchain
      config). `new` stages the project and never replaces a file. `export` replaces
      its staged tree only when the recorded SHA-256s match, else it reports a
      conflict unless `--force`. `dist/` is staged and swapped. No in-file "generated
      by" header: the record does that job, and a header would break JSON or binary
      outputs.

### C7.3 — Cancellation and locks (§43, §44)

- [x] Ctrl-C/SIGTERM two-stage in `run` (kill build tree, ask app to quit, force).
- [x] The same cancellation for every long command. `build`, `package`, `test`,
      `check`, `new`, `profile`, `inspect events` and `ios use` already caught Ctrl-C.
      `android use` now does too, once its licenses are accepted: the download or
      unpack at hand stops, what arrived is kept to resume, and a half-unpacked
      component is removed. Before, curl in its own group outlived the CLI. Packaging
      tools (`codesign`, `apksigner`, `zipalign`…) run through the CLI runner, so an
      interrupt ends them and the build reports `cancelled`. `export` is in-process
      and quick. Exit 130 with `summary{status:"cancelled"}`, locks released.
- [x] Lock kinds `build-<target>`, `package-<target>`, `dev-<target>[-<device>]`. A
      held lock fails a command at once with `ENV_LOCK_HELD`, its holder and path;
      `build`/`package` used to report it as `C0011`. `run`'s own rebuilds wait for
      the build lock and can be cancelled. Lock order is always
      dev/package → build, so nothing deadlocks.

### C7.4 — Human UX (§52–§55)

- [x] Progress (§54): one exit, `Output::advance`/`settle`. On a TTY it is one live
      line with the elapsed time, redrawn at most every 200 ms. Elsewhere it is plain
      lines at most every 10 s, or `progress` events at most once a second under
      `--json`; nothing under `--quiet`. The android and iOS downloads used to draw
      their own lines. `build` ends with `Built … in N s`, plus the stage timings
      under `--verbose`.
- [x] Prompts (§55): one rule (`can_ask`: human output, TTY on stdin and stderr, no
      `CI`). Every prompt has a flag or command equivalent: `--accept-licenses`,
      `--download`, `--device`, `… create phone`. `tests/prompts.rs` runs each
      prompting command with "yes" on a piped stdin and asserts it fails structurally
      without reading it.

### C7.5 — AI contract and performance (§56–§58, §64–§66)

- [x] AI-friendly commands (§56): `tests/json.rs` runs `check`, `schema`,
      `schema --search`, `dump hir`, `explain`, `fmt --check` and `config show` twice.
      The event streams must be identical except for the envelope clock, the session
      and `elapsed_ms`, and contain no color codes. Diagnostics carry `next` commands
      as fixes (§53).
- [x] Startup (§64): `tests/startup.rs` runs `--version`, `--help`, `completion` and
      `explain` in a directory with a broken `Viso.toml`, an empty `PATH` and no home.
      All succeed and nothing is created. `viso --version` must come in under a 250 ms
      budget (best of 5, debug build). That budget is a hypothesis until measured on
      CI hardware.
- [x] Incremental build (§65): a no-change `build` reuses its staged output.
      Staging (bundle, wasm-bindgen, wasm-opt, compression, ad-hoc signing, APK) is
      keyed on the executable's size and mtime, the target, profile, web settings,
      SDK, the CLI binary and the web tools on `PATH`. The key is recorded in
      `out.staged`. When `dist/` holds exactly those artifacts and manifest and
      nothing else, it is not copied again. The artifacts report `cached: true` and
      the human line ends `, unchanged`. `package` always packages afresh.
- [x] No unnecessary serialization (§66): audited. Every decode is at a process
      (cargo, simctl), socket (dev channel) or file (traces, goldens, records)
      boundary; services pass typed values in-process.

### C7.6 — Testing the CLI itself (§59–§63)

- [x] Parser golden tests (§59): `tests/golden.rs`. `help.txt` holds every help
      page (the list is discovered from the parser). `usage.txt` holds every usage
      error class with exit 2: unknown command, missing subcommand, unknown flag,
      conflicting flags, a bad value, a bad possible value, a wrong target form
      (`run macos`, `--target ios`) and a bad positional (`config set a b`), plus
      its JSON form with the `--help` fix.
- [x] Integration tests (§60): new→check/build headless/test (`tests/new.rs`),
      `fmt --check` (`fmt.rs`), schema (`schema.rs`), invalid `.vs` (`check.rs`),
      web-dom and web-gpu builds (`build.rs`), html and solid exports (`export.rs`),
      `package --dry-run` (`package.rs`), `snapshot compare` (`snapshot.rs`).
- [x] Fake backends (§61): the fake SDKs live in the crate's own tests
      (`deploy/android_fake.rs`, `deploy/ios_fake.rs`), so no env var exists that a
      shipped binary would honour. Both platforms cover a session, a crash, a device
      that goes away mid-session, a boot that fails and a missing SDK. Signing errors
      are covered in `tests/package.rs`. An Android emulator that stops at start now
      reports its own last lines at once; before, it waited 120 s and said only
      "did not start".
- [x] JSON contract tests (§62): envelope fields, `seq` monotonic, `summary` last and
      exactly once, `exit_code` equal to the process exit, every `code` in the
      registry (C2.3) — `tests/json.rs` over a dozen commands, success and failure.
- [x] Ctrl-C/cleanup tests (§63). `tests/cancel.rs` interrupts `build`, `package`
      and `test` while a build script hangs. It asserts exit 130, a `cancelled`
      summary, the script's process gone, no lock left and no staged `dist/` entry.
      It also checks that a held lock fails at once, naming its holder. `run` is
      covered by `tests/device.rs` and `check --watch` by `tests/check.rs`.

---

## C8 — Definition of Done (§76)

- [x] Project: `new` generates a minimal runnable project; discovery stable; config
      precedence tested; `doctor` explains environment gaps.
- [x] Develop: `check/build/run` cover the host; `run` has watcher and hot reload;
      Ctrl-C cleans up; headless usable in CI.
- [x] Mobile: `run` means the desktop host only; `run ios/android` targets only
      simulator/emulator; `--device` accepts only Viso virtual-device profiles;
      `android list/use/doctor/emulator/adb` and `ios list/use/doctor/simulator` manage
      their environments alone; no signing, provisioning or physical device in the dev
      loop; no `sdkmanager`/`avdmanager`/`simctl` in ordinary workflows.
- [ ] Web: `web-gpu`, `web-dom`, `web-hybrid`, `serve`, capability diagnostics.
      (`web-gpu`, `web-dom`, `web-hybrid` and `serve` done; the platform-gated rows of
      the capability table remain, C4.4.)
- [x] Language: `fmt`, `check`, `schema`, `explain`, `dump`, `lsp`.
- [x] Test/Debug: `test`, `snapshot`, `inspect`, `profile` (`studio` dropped, see above).
- [x] Delivery: `package`, artifact manifest, `export html`, `export solid`.
      (Signing with real credentials and the Windows host run are the open
      verification items of C5.2.)
- [x] Automation: `--json` on every command but `lsp`/`studio`/`completion` per
      §34–§37; stable exit codes; no prompts off a TTY; Studio/IDE/AI share services
      and protocol; the CLI duplicates no compiler/build/packager core.

## C9 — User guide

- [x] `docs/cli/` usage guide for the documentation site: install, the first project,
      the dev loop (`run`, hot reload, `inspect`, `profile`), checking and formatting,
      testing and snapshots, mobile (`android`/`ios` environments, emulators and
      simulators), web (`web-gpu`/`web-dom`, `serve`), building, packaging and
      signing, export, automation (`--json` events, exit codes, `explain`), config
      precedence, and troubleshooting with `doctor`. Task-oriented pages with copyable
      commands, each checked against the CLI as built.
