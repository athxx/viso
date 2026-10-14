# Viso CLI — C0 → C1 → C2 → C3 → C4 → C5 → C6

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
  `viso-package`; device services in `tools/cli/src/device/` until Studio needs them,
  then a shared tooling crate.
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

### C1.2 — `viso new` (§8)

- [ ] Templates `app`, `game`, `web`, `library` embedded in the binary (no network);
      each a Cargo package plus `Viso.toml`, `src/main.rs`, `ui/*.vs`, `tests/ui/`,
      `.gitignore`; `library` has no binary.
- [ ] Options `--template`, `--name` (package name validated as a Cargo name, C-code
      on invalid), `--edition`, `--no-git` (default runs `git init` when git exists,
      a missing git is a warning), `--force-empty-dir`.
- [ ] `viso new <path>` refuses a non-empty directory (exit 2, lists what is there);
      `viso new .` works in an empty directory and takes the directory name.
- [ ] Generated files written atomically (temp dir, then rename) so a failure leaves
      nothing behind (§51).
- [ ] JSON `result{path,template,files}`; human output prints the next commands.
- [ ] Acceptance: for each template `new → check → build headless → test ui` passes
      (integration test, offline, uses the workspace's crates via a path override).

### C1.3 — `viso doctor` (§9)

- [ ] A `Check { id, title, status: ok|warn|error|skip, detail, fix? }` list built
      from probes: Rust toolchain and targets, cargo, the project (manifest, members,
      language version), GPU backend availability on the host, git; one
      `doctor_check` event each, human table, exit 3 on any error.
- [ ] `doctor <target>` adds that target's probes (delegating to `android doctor`,
      `ios doctor`, the web probes of C4) and nothing else.
- [ ] `--fix` performs only fixes that are local and reversible (`rustup target add`,
      creating `target/viso/`), prints each before running it, and never downloads an
      SDK (that is `android use` / `ios use`; §9.3).
- [ ] Probes run in parallel with a per-probe timeout; a hung tool is `error` with the
      timeout, not a hung doctor.

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

- [ ] `check <target>`: target capability checks (a GPU-only node on `web-dom`, a
      platform-gated API on a target that lacks it) on top of the fast default (§17.1).
- [ ] `--watch`: reuses the dev session watcher; re-checks on change, prints a
      per-batch summary, emits one `summary` per batch under `--json`, Ctrl-C exits
      130.
- [ ] Optional Rust side: `--cargo` runs `cargo check` for the project (default off,
      §17.2 keeps check fast).
- [ ] A schema-enum property value names a variant of its enum: `semantics.role:
      Role::bogus` or `justify: Justify::bogus` passes `viso check` today (the schema
      enums `Role`, `Justify`, `LiveRegion`, … have no registered variants); only the
      mount of a view rejects an unknown role.

### C1.6 — `viso build` (§14)

- [x] `build [<target>]` with profiles `dev`, `release`, `shipping` (`--release` is
      `--profile release`; `--release` with another `--profile` is a usage error);
      unknown profile C0009; `hot_reload` refused in release/shipping (C0008).
- [x] One Build Service (`tools/cli/src/build/`) shared by `run` and `build`
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
- [ ] `--inspect` and `--profile-frame` land with the inspector channel and frame
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
- [ ] The rest of `.vs` semantics: `semantics.hint`, `value`, `live`, `hidden`,
      `heading_level`, the `tab`/`radio` roles of `Tabs` pages and `RadioGroup`
      items, and the default roles of `Modal`/`Sheet`/`Toast`/`NavigationStack`.
- [x] Filters (§22.4): `--filter <pattern>`, `--exact`, `--fail-fast`, `--nocapture`,
      `--update-snapshots`, `--target <t>`, `--device <profile>` (C3). (`--target`
      takes `headless`; `--update-snapshots` lands with C2.6, `--device` with C3.5.)
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

- [ ] All query forms of §18.1 (`Widget`, `Widget.prop`, `module::Item`, `--list
      widgets|components|natives|types`), project components included with their
      source origin (`file:line`, §18.3), framework items marked `framework`.
- [ ] JSON schema output stable and versioned (`schema_version`), golden-tested
      (§58).

### C2.3 — `viso explain` (§19)

- [ ] A diagnostics registry in `viso-dsl`: every code (`E####`, `Lex####`,
      `Parse####`) with title, long explanation, a failing example and the fix; one
      table, generated docs read from it.
- [ ] A test enumerates every code the compiler can emit and fails if one is missing
      from the registry (and vice versa); same for CLI codes (`C####`, `CLI_*`,
      `ENV_*`, `BUILD_*`, `TEST_*`, `DEVICE_*`, …) in a CLI-side registry.
- [ ] `viso explain <code>` prints it (no project needed), unknown code exit 2 with
      nearest codes; `--json` gives `result{code,title,explanation,example,fix}`;
      `--list` lists all codes.

### C2.4 — `viso dump` (§20)

- [ ] Stable textual and JSON dumps from `viso-dsl` for `ast`, `hir`, `ui-ir`,
      `reactive-ir` (binding IR + dirty map), `behavior-ir` (existing `dump()`
      generalized), `shader-ir`, `system-ir`, `module-graph`; one `Dump` trait, no
      `Debug` formatting as output.
- [ ] `viso dump <kind> [<file>] [--symbol <path>] [--out <file>]`; source spans
      included; dumping an invalid file prints diagnostics and dumps what lowered.
- [ ] Golden tests per kind on a fixture project; the dump format is documented as
      unstable-for-humans but its JSON keys versioned.

### C2.5 — `viso lsp` (§21)

- [ ] `viso lsp` execs `viso-lsp` (found next to `viso`, then on `PATH`), forwarding
      stdio and args; missing binary is exit 3 with the install hint; no `--json`.
- [ ] `viso-lsp` uses the shared formatter (C1.4) and the diagnostics registry for
      hover on codes.

### C2.6 — `viso snapshot` headless (§23)

- [ ] `snapshot capture <scenario|component>`: render headless at the test surface,
      write `tests/snapshots/<name>.<target>.png` plus a sidecar (tree hash, build id,
      renderer, size).
- [ ] The software raster's CPU shader programs run without an allocation per
      intrinsic call (compiled to closures or a flat register program): a custom
      shader at 1280×720 now takes ~80 ms a frame with the raster crates optimized
      (they are, in an unoptimized headless build), ~0.7 s without.
- [ ] `snapshot compare`: perceptual + exact compare with a per-snapshot tolerance,
      diff image into `target/viso/snapshots/`, exit 1 on mismatch, `test`-shaped
      events.
- [ ] `snapshot update [--filter]`: rewrites only mismatched snapshots, reports each.
- [ ] `expect snapshot "<name>"` steps in UI scenarios and `test ui
      --update-snapshots`, through the same compare.
- [ ] `--target headless` default; other targets come with C3/C4.

### C2.7 — `viso inspect query` (§24.3)

- [ ] `inspect query <selector>` against a headless instance (or a running session in
      C6): selector syntax (type, `#id`, `.class`, `[prop=value]`, descendant), JSON
      result of matched nodes with layout rect, props and source span.
- [ ] Shared Inspector Service type used by tests (C1.9), Studio and the CLI.

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

- [ ] `android list`: installed and available API levels with their component set
      (platform, build-tools, platform-tools, emulator, system image per host ABI,
      NDK), marking the active one; `--offline` lists installed only.
- [ ] `android use <api>`: download the pinned component set into
      `~/.viso/android/sdk/` — each archive's SHA-256 verified against the pinned
      table, extracted to a temp dir, moved into place atomically, only the files a
      build needs kept (makepad's selective expansion); resumable; progress events;
      accepts the license by an explicit `--accept-licenses` or a TTY prompt, never
      silently; `--offline` fails with exit 3 when anything is missing.
- [ ] Active SDK recorded in `~/.viso/android/config.toml`; resolution order
      `ANDROID_HOME`/`ANDROID_SDK_ROOT` > active Viso SDK > default locations,
      reported with origin.
- [ ] `android doctor`: SDK, NDK, build-tools, platform-tools, emulator binary,
      system image for the host ABI, hardware acceleration (HVF/KVM/WHPX), Java only
      where a step needs it, rustup targets; `doctor_check` events; each failure's
      fix is a `viso` command.
- [ ] Unblocks the Android API 35/36 image install noted in `todo_hotreload.md`.

### C3.3 — `viso android adb` and logs (§11.5, §11.6)

- [ ] `android adb [--device <profile>] -- <args>`: resolves the profile to the
      running emulator's serial, runs the SDK's `adb -s <serial>` with the args
      verbatim, exit code passed through; not running is exit 3 with the start
      command.
- [ ] `android logs [--device]`: `logcat` filtered to the app's pid, as `log` events
      under `--json`.

### C3.4 — `viso ios list|use|doctor` (§12.1–§12.3)

- [ ] `ios list`: installed simulator runtimes and device types (`simctl list -j`),
      the active one marked; downloadable runtimes when Xcode can report them.
- [ ] `ios use <runtime>`: sets the default runtime for new profiles; a runtime not
      installed is installed through `xcodebuild -downloadPlatform iOS` only with
      explicit consent (flag or TTY prompt); never touches Xcode itself.
- [ ] `ios doctor`: Xcode and command line tools selection, license accepted,
      simulator runtime, `aarch64-apple-ios-sim` target, disk space for a runtime;
      on a non-macOS host every `ios` command is exit 3 with one clear diagnostic.

### C3.5 — Mobile `build`, `test`, `logs`

- [x] `build ios|android` through the Build Service (C1.6): simulator `.app` (ad-hoc)
      / emulator APK (dev APK in dev, release APK otherwise); device signing is C5.
- [ ] `test --target ios|android --device <profile>`: UI tests on the simulator or
      emulator through the same runner protocol as headless (C1.9).
- [ ] `ios logs [--device]`: the app's `log stream` as `log` events.
- [ ] Fake iOS backend (simctl replaced by a scripted fake) beside the Android one,
      covering boot failure, disconnect mid-session and SDK missing (§61).

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

### C4.2 — web-gpu remaining

- [x] `build web-gpu` through the Build Service with the optimization policy (C1.6):
      a static directory with precompressed files.
- [ ] Hashed asset names in the web bundle (`app.<hash>.js`, `app_bg.<hash>.wasm`,
      the page naming them) so a server can cache them forever.
- [ ] `[profile.<name>.web] split` and `threads`: a startup/lazy module split and a
      shared-memory threaded module (`auto` resolves to off until then).
- [ ] `serve` security headers of §15.3 (COOP/COEP when threads are on, CSP, `nosniff`,
      correct MIME for `.wasm`), served precompressed when the client accepts it.
- [ ] `serve --https`: self-signed loopback/LAN certificate generated and cached under
      `~/.viso/web/` (needs a TLS dependency decision — rustls; pending user call).
- [ ] Page-side `print`/`eprintln` log API (pending user call, see `todo_hotreload.md`).
- [ ] `test web --target web-gpu`: headless Chrome/WebDriver runner when a browser is
      found, `skip` otherwise.

### C4.3 — web-dom and web-hybrid

- [ ] Depends on the DOM runtime backend (`Viso_Architecture.md`/`Viso_Rendering.md`);
      the CLI side lands when that backend exists: build target, bundle, `run`,
      `serve`, `check web-dom` capability rules. Until then the targets stay C0011.
- [ ] `web-hybrid` (§32): DOM shell with GPU islands; the bundle carries both runtimes
      and the capability split.

### C4.4 — Web capability diagnostics (§33)

- [ ] Capability table per target (GPU-only nodes, custom shaders, threads, file
      system, native libraries) in the compiler, consumed by `check <web-target>` and
      `build`; each diagnostic names the node, the target and the alternative
      (`WEB_*` codes registered in C2.3).

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

- [ ] `package [<target>]`, default profile `shipping`; `--profile dev` is exit 2
      (§2.2); host targets: macOS `.app` + `.dmg`/zip, Windows/Linux archive; iOS
      `.ipa`; Android `.aab` and `.apk`; web directory + archive.
- [ ] Package metadata (§27.1) from `[package]`/`[ios]`/`[android]` in `Viso.toml`:
      id, display name, version, build number, icons, permissions, orientation;
      validated (C diagnostics) before building.
- [ ] Signing (§27.2): `--sign auto|required|off`; identities, keystores and
      passwords only from env/keychain/files named in config, never on the command
      line or in logs (§49); `required` with no identity is exit 6.
- [ ] `--dry-run` (§27.3): prints the plan (build, steps, signing identity chosen,
      output paths) and validates metadata without building.
- [ ] Package lock `package-<target>`; output `dist/<target>/` with
      `artifact.json` (§27.4: files with SHA-256, size, target, profile, build id,
      signing summary, toolchain versions); `artifact` events; exit 6 on
      signing/archive failure.

### C5.3 — `viso export html` (§28, §29, §31)

- [ ] Capability analysis (§28.2) shared with C4.4: every node classified static,
      dynamic or GPU-only before any file is written.
- [ ] Layout lowering to HTML + CSS (§29.2): flex/stack/grid mapping, text, images,
      theme tokens as CSS variables; output deterministic (stable ordering, no
      timestamps).
- [ ] `--static` (§29.1): a dynamic binding is `EXPORT_HTML_DYNAMIC_REQUIRED`, a
      GPU-only node is `EXPORT_HTML_GPU_ONLY_NODE`, each with span and alternative;
      exit 1.
- [ ] Generated-file safety (§51): writes only into the export dir, refuses to
      overwrite files without the generated header unless `--force`.

### C5.4 — `viso export solid` (§30)

- [ ] SolidJS component source from UI IR + binding IR (§30.1): signals for state,
      memos for derived values, event handlers; the state example of §30.2 as a
      golden test.
- [ ] SolidJS is never a Viso core dependency (§30.3): the exporter emits source and a
      `package.json` stub only.
- [ ] Ownership (§30.4): generated header, regenerate-in-place rules, user edits
      detected and preserved by refusing to overwrite.

---

## C6 — Full observability (§24–§26, §75)

Goal: a running app (host, simulator, emulator, browser) can be inspected, profiled
and traced from the CLI and from Studio over the same services.

- [ ] `inspect --run [<target>]` / `inspect --attach <session>`: inspector channel on
      the dev runtime transport; tree, props, layout, styles, events; `query` works
      against it (C2.7).
- [ ] Inspector capabilities negotiated (§24.2): what a runtime reports it supports.
- [ ] `profile [<target>]` (§25): frame time, CPU/GPU split, layout/paint/reactive
      counts, allocations, draw calls; `profile` events; `--frames <n>`,
      `--duration`.
- [ ] Trace output (§25.3): `--output trace.ende` (Ende binary trace), `--chrome`
      (Chrome trace JSON); traces under `target/viso/traces/`.
- [ ] `run --profile-frame` overlay and per-frame `profile` events.
- [ ] `run --inspect`: the session opens the inspector channel to the app it
      launches, the same one `inspect --attach` uses.
- [ ] Device profiles for mobile performance (simulator vs emulator caveats reported
      in the result).
- [ ] `viso studio [--target]` (§26): launches Studio against the project, sharing the
      dev session service (blocked until Studio exists; the dev session moves into a
      shared tooling crate first, see `todo_hotreload.md`).

---

## C7 — Cross-cutting contracts (grows with every phase)

### C7.1 — JSON protocol (§34–§37)

- [x] Event envelope and stream; `diagnostic`, `progress`, `device`, `server`, `dev`,
      `runtime`, `log`, `trace`, `test`, `result`, `artifact`, `summary`.
- [ ] Remaining event types: `profile` (C6), `doctor_check`
      (C1.3); every payload documented in `Viso_CLI.md` with an example.
- [ ] Every command except `lsp`, `studio`, `completion` supports `--json`; a test
      runs each command with `--json` and checks the stream (C7.6).
- [ ] stdout carries only events under `--json` (child output becomes `log`),
      human output never writes events; a test closes stdout early and the CLI exits
      cleanly without a panic (`EPIPE`).

### C7.2 — Reliability and security (§48–§51)

- [ ] External processes (§48): one runner — explicit program path, args as a list
      (never a shell string), env allow-list, timeout, process-group kill on cancel,
      captured output on failure, `--verbose` echoes the command line with secrets
      redacted.
- [ ] Secrets (§49): a `Secret` type that never `Display`s; env keys matching
      `*TOKEN*`, `*PASSWORD*`, `*SECRET*`, `*KEY*` redacted in logs and `--verbose`.
- [ ] Network downloads (§50): HTTPS only, pinned SHA-256, size limit, temp file +
      rename, retries with backoff, `--offline` honoured, proxy env respected.
- [ ] Generated-file safety (§51): one helper for atomic writes and "generated by
      viso" headers, used by `new`, `fmt`, `export`, `package`.

### C7.3 — Cancellation and locks (§43, §44)

- [x] Ctrl-C/SIGTERM two-stage in `run` (kill build tree, ask app to quit, force).
- [ ] The same cancellation for every long command (`build`, `test`, `package`,
      `android use`, `export`): first Ctrl-C stops work and cleans temp files, second
      forces; locks released; partial downloads kept for resume; exit 130 with
      `summary{status:"cancelled"}`.
- [ ] Lock kinds `build-<target>`, `package-<target>` beside `dev-<target>[-<device>]`;
      a command that needs a build while `run` holds the dev lock reuses or waits per
      §44, never deadlocks.

### C7.4 — Human UX (§52–§55)

- [ ] Progress (§54): one live line on a TTY (stage, elapsed), plain lines otherwise,
      nothing under `--quiet`; stage timings in the final line.
- [ ] Prompts (§55): only on a TTY with no `CI` and no `--json`; every prompt has a
      flag equivalent; a test runs each prompting command non-TTY and asserts no read
      from stdin.

### C7.5 — AI contract and performance (§56–§58, §64–§66)

- [ ] AI-friendly commands (§56): deterministic output order, no timestamps in
      `result`, `--json` diagnostics with suggested commands and fixes; nothing
      requires parsing colored text (§57).
- [ ] Startup (§64): `--help`, `--version`, `completion`, `explain` load no project,
      compiler or SDK; a test measures `viso --version` under a budget (release, with
      a hypothesis label until measured on CI hardware).
- [ ] Incremental build (§65): a no-change `build` reuses the `BuildId` artifact
      without invoking cargo's build steps beyond its fresh check; reported as
      `cached` in the artifact summary.
- [ ] No unnecessary serialization (§66): services pass typed values in-process; JSON
      is produced only at the output edge.

### C7.6 — Testing the CLI itself (§59–§63)

- [ ] Parser golden tests (§59): `--help` of every command snapshot, every usage error
      class (unknown flag, conflict, bad positional, wrong target form) with its exit
      code.
- [ ] Integration tests (§60): new→check, new→build headless, new→test ui,
      fmt --check, schema, invalid `.vs`, web-dom/web-gpu build, html/solid export,
      package --dry-run, snapshot compare.
- [ ] Fake backends (§61): fake iOS and Android (existing Android), disconnected
      device, signing error, SDK missing — selected by an env var only test builds
      honour.
- [ ] JSON contract tests (§62): envelope fields, `seq` monotonic, `summary` last and
      exactly once, `exit_code` equal to the process exit, every `code` in the
      registry (C2.3).
- [ ] Ctrl-C/cleanup tests (§63): for `run`, `build`, `test`, `package`: signal
      mid-stage, assert no child left, lock released, temp files gone, exit 130.

---

## C8 — Definition of Done (§76)

- [ ] Project: `new` generates a minimal runnable project; discovery stable; config
      precedence tested; `doctor` explains environment gaps.
- [ ] Develop: `check/build/run` cover the host; `run` has watcher and hot reload;
      Ctrl-C cleans up; headless usable in CI.
- [ ] Mobile: `run` means the desktop host only; `run ios/android` targets only
      simulator/emulator; `--device` accepts only Viso virtual-device profiles;
      `android list/use/doctor/emulator/adb` and `ios list/use/doctor/simulator` manage
      their environments alone; no signing, provisioning or physical device in the dev
      loop; no `sdkmanager`/`avdmanager`/`simctl` in ordinary workflows.
- [ ] Web: `web-gpu`, `web-dom`, `web-hybrid`, `serve`, capability diagnostics.
- [ ] Language: `fmt`, `check`, `schema`, `explain`, `dump`, `lsp`.
- [ ] Test/Debug: `test`, `snapshot`, `inspect`, `profile`, `studio`.
- [ ] Delivery: `package`, artifact manifest, `export html`, `export solid`.
- [ ] Automation: `--json` on every command but `lsp`/`studio`/`completion` per
      §34–§37; stable exit codes; no prompts off a TTY; Studio/IDE/AI share services
      and protocol; the CLI duplicates no compiler/build/packager core.
