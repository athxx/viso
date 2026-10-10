# Viso Hot Reload — H0 → H1 → H2 → H3 → H4 → H5 → H6 → H7 → H8

Construction order follows `Viso_Hot_Reload.md` Part XXII (§65–§70): H0 closes the
dev-only build contract, H1 is Phase A on the target architecture (host watches and
compiles, the app receives typed patches), H2 is Phase B, H3–H5 are Phase C, H6 is
Phase D, H7 is Phase E, H8 is Phase F. H9 (acceptance, §61–§64, §71) grows with every
phase. A later phase never becomes a prerequisite of an earlier phase's vertical slice.
Work one verifiable sub-unit at a time; a semantic or protocol change updates
`Viso_Hot_Reload.md` (and `Viso_CLI.md` where the CLI surface moves) in the same change,
and an architecture decision (host/runtime split, wire protocol, warm restart) updates
ADR-0015 or adds an ADR. A perf claim needs a release measurement; otherwise it is
labeled a hypothesis.

Landing points follow the dependency rules:

- `viso-dsl` is the only compiler and runs **on the host only** (the dev session in
  `viso run`); it produces patches (§56) and never handles a connection.
- The runtime dev layer (§55, §57) — handshake, patch decode/validate, staging, commit,
  scoped reset, snapshot hooks, ACK/NACK — lives in `viso-view::dev` (UI/behavior
  domain) with thin domain hooks in `viso-behavior` (game), `viso-shader`/`viso-render`
  (pipelines) and the facade (frame boundary, transport, overlay), all behind the
  `hot-reload` feature. `runtime` never depends on `dsl`.
- The host dev session (§4, §54) is a module tree in `tools/cli/src/dev/`; it moves to
  a shared tooling crate only when Studio needs it.

Reference read first (memory rule): makepad `platform/src/live_reload.rs`,
`libs/live_reload_core`, `libs/filesystem_watcher`, `platform/studio/src/studio.rs`,
`platform/src/draw_shader.rs`, `tools/cargo_makepad/src/wasm/compile.rs`. Take: the
source-keyed shader cache (unchanged shaders reuse their pipeline), per-tick coalescing
of the latest content per file, one dispatch point for every incoming dev message.
Avoid: parse-only validation that commits runtime-invalid edits, `redraw_all` after a
reload, one bad file aborting other files' valid edits, positional block matching,
unbounded shader growth across a long session, mobile reaching the host over LAN.

Gate for every section: `cargo xtask check-deps` · `cargo fmt --all -- --check` ·
`cargo clippy --workspace --all-targets -- -D warnings` ·
`cargo clippy -p viso --all-targets --features hot-reload -- -D warnings` ·
`cargo test --workspace` · `cargo test -p viso --features hot-reload` ·
`cargo xtask check-targets`.

Not touched by this plan while the font work runs in another process: the text/font
crates. §23 (font patch) is listed in H4 and waits for that work.

---

## H0 — Dev-only build contract (§1, §1.1, §1.2, §58–§60, §64)

Goal: a release or shipping binary has no dev code at all — not disabled, absent —
and the dev build is exercised in CI.

### H0.1 — What exists

- [x] `hot-reload` cargo feature on `viso` and `viso-view`; the facade's session,
      overlay, wakeup, frame-boundary reload and close are `cfg`-gated, so the
      release frame has no hot-reload branch.
- [x] `view!` mount registration expands to nothing without the feature; no `.vs`
      source text reaches the binary.
- [x] `Viso.toml` rejects `hot_reload` under `[profile.release]`/`[profile.shipping]`
      (`ConfigCode::HotReloadInRelease`), warns under dev; `ProfileConfig` has no
      such field.

### H0.2 — Close the build graph

- [x] `viso-dsl` becomes a dev-only dependency of the facade: optional, enabled only by
      `hot-reload` (`dep:viso-dsl`), a dev-dependency for the facade's DSL tests; dropped
      from the facade entirely once H1.4 moves compilation to the host. A default build
      links no compiler (only the `ui!`/`view!` proc-macro uses it, on the host).
- [x] The wire codec (`viso-dsl::hotreload::event`) stays put until H1.2: it is built on
      `viso-dsl`'s `Diagnostic`, and the optional dependency already keeps it out of the
      release graph. H1.2 replaces it with `viso-view::dev::wire` and deletes it.
- [x] Build-time guard: `crates/viso/build.rs` fails the build when `hot-reload` meets
      `VISO_PROFILE=release|shipping`. It keys on the Viso artifact profile, not Cargo's
      `PROFILE` (an optimized dev build — the `edit_to_pixels` measurement — is still a
      dev artifact). `viso run` builds with `VISO_PROFILE=dev`; the future
      `viso build/package` must set the artifact's profile (H0.3 tests the guard).
- [x] `RedrawReason::HotReload` kept: an architecture-listed reason (§12), one bit,
      never set by a release artifact, so `runtime` pays nothing for it.

### H0.3 — CI and release absence (§64)

- [x] CI job building and testing `viso` with `--features hot-reload` (today the
      feature-gated tests never run in CI), on macOS, Linux and Windows
      (`.github/workflows/ci.yml` `hot-reload`: clippy + tests).
- [x] Release-absence test (`cargo xtask check-release-absence [-p] [--no-launch]`,
      `xtask/src/absence.rs`): builds the `view!` app `viso-example-i18n` in release,
      then asserts
  - [x] no symbol or string from the dev layer (`VISO_DEV_RUNTIME`, `VISO_DEV_TOKEN`,
        the `viso-dev-link` thread, `DevLink`, the overlay text) in the binary — each
        marker first required in a `hot-reload` control build, so a stale marker fails
        instead of passing vacuously;
  - [x] no `viso_dsl` symbol at all (symbols are read from the executable: macOS and
        Linux; a Windows executable keeps them in its PDB);
  - [x] launched with `VISO_DEV_RUNTIME`/`VISO_DEV_TOKEN` set and a loopback listener,
        the control connects (166 ms here) and the release binary runs 3× that (≥ 5 s)
        without connecting; an early exit fails. The variable names are absent from
        the binary, so it cannot read them.
  - [x] CI: macOS with launch, Linux `--no-launch` (`release-absence` job).
- [x] Guard test: the same xtask builds with `VISO_PROFILE=shipping` and requires the
      guard's refusal; the control build is the `VISO_PROFILE=dev` case.
- [x] Release frame-loop parity (`benches/frame_loop.rs`, a `view!` app toggling an `if`
      branch every frame, release, Apple Silicon): 3.83 µs/frame without the feature,
      3.78 µs/frame with it (session idle) — within noise; startup +32 µs (adopt +
      watcher). Recorded in §1.2.
- [x] Spec: §58 names the feature, the guard and the absence test; §64 records what the
      test checks and what it does not cover yet; §1.2 records the parity measurement.

### Done

- [x] A release build of an app with `view!` files contains no dev transport, patch
      engine, snapshot endpoint or compiler, proven by the absence test in CI. Passes
      locally on macOS (launch included); the patch engine and snapshot endpoint do
      not exist yet, and each adds its marker when it lands (H1.2, H6.2).

---

## H1 — Phase A on the target architecture: host compiles, app applies (§2–§11, §33–§37, §65)

Goal: `viso run` watches and compiles on the host, sends a typed, bounded, revisioned
patch, and the app commits it at a frame boundary or NACKs and keeps last-good. The
app parses no `.vs` source (anti-pattern B.2).

### H1.1 — What exists (in-app compile, the reduced design)

- [x] Pure stages `plan` → `diff` → `migrate` and an infallible `commit` at
      `FlushStateTransactions`; last-good kept on any failure (`dsl/src/hotreload/`).
- [x] `view!` mount records: file, source, origin, root, state cells by `SymbolId`
      key, behavior host, static `NodeKey`s (`view/src/mounts.rs`).
- [x] App-side session: adopt a window's mounts after its build, stage on wakeup,
      commit at the frame boundary, drop mounts of closed windows, one compile per
      edit committed to every mount (`viso/src/hot_reload/mod.rs`).
- [x] Watcher: kqueue / inotify / `ReadDirectoryChangesW` over each file's directory
      with a poll fallback; quiet windows 5 ms / 0 / 5 ms; content-hash dedupe;
      read once at tracking start (`viso/src/hot_reload/watch/`).
- [x] Per-file last-good candidate and revision; `ReloadEvent` with stage, outcome,
      counts and diagnostics; in-app failure overlay (§37.1).
- [x] `viso run` (host desktop): dev build with `viso/hot-reload`, loopback listener,
      `VISO_DEV_RUNTIME` + 128-bit `VISO_DEV_TOKEN`, ende frames ≤ 4 MiB, hello with
      protocol version, events relayed as human lines or `--json` `dev` events.
- [x] Release edit-to-pixels on macOS: median 6.6 ms (detect 5.8 ms, pipeline 0.8 ms).

### H1.2 — Protocol (§33–§37, §46)

- [x] `viso-view::dev::wire` (behind `viso-view/hot-reload`): one ende-binary message
      family per direction (`HostMessage`, `RuntimeMessage`) with stable tags and
      `DEV_PROTOCOL_VERSION` 2; bounded decode (token 64 B, ≤ 64 NACK codes of ≤ 32 B,
      log 16 KiB, ≤ 4096 sections, every count also ≤ the bytes left; no nesting, so
      fixed depth), frame cap 4 MiB refused before the body is read, malformed input a
      typed `WireError`.
  - [x] Runtime speaks first: `RuntimeHello { protocol_version, token, dev_session,
        runtime_session, build_id, current_revision, schema_fingerprint,
        capabilities, target }`; the host answers `HostHello { protocol_version,
        dev_session, runtime_session, project_fingerprint, expected_build_id }` or
        `Reject`. The token moved from the host's hello to the runtime's: the
        connecting side proves itself, and the host sends nothing to an unproven
        connection (§34).
  - [x] A protocol, session or build mismatch is a `Reject` with its reason and the CLI
        asks for a rebuild; a hello and a reject keep their version-first prefix in
        every version, so another version is named, never guessed.
  - [x] Handshake binds token, dev session, runtime session and build (the CLI passes
        `VISO_DEV_SESSION` and `VISO_DEV_BUILD`); the host's hello carries the project
        fingerprint; a wrong token is dropped unanswered (§46 gap closed).
  - [x] `PatchBundle { dev_session, target_runtime, base_revision, next_revision,
        build_id, sections }` (§35): at most one section per domain in tag order,
        stable domain tags (`ui module state system shader resource`), fixed-width
        128-bit ids, no field-name strings. Each domain's payload lands with the
        phase that applies it (H1.4 ui/module/state); until then its tag decodes as
        `UnsupportedDomain` and is NACKed.
  - [x] `PatchAck { revision, applied_domains, scoped_resets, timings }` and
        `PatchNack { base_revision, candidate_revision, stage, diagnostic_codes,
        last_good_revision }` (§37), every §51 stage on the wire; NACK codes
        `NACK_UNKNOWN_SESSION`, `NACK_BUILD_MISMATCH`, `NACK_REVISION_MISMATCH`,
        `NACK_REVISION_ORDER`, `NACK_UNSUPPORTED_DOMAIN`, `NACK_MALFORMED`.
  - [x] Release-absence markers: the new env names and `NACK_REVISION_MISMATCH`
        (present in the control build).
  - [x] Revision rule (`RuntimeIdentity::check`, before staging): session, launch,
        build, `base == current` else `NACK_REVISION_MISMATCH`, `next > base` else
        `NACK_REVISION_ORDER`, advertised domains only; a patch arriving behind a
        staged one chains on its `next_revision`; staged patches commit at the frame
        boundary in order, each ACKed (§36).
  - [x] The in-app compile result travels as `RuntimeMessage::InAppReload` (the
        `ReloadEvent` ende bytes, `viso-dsl` keeping only that codec) until the host
        compiles; the frame codec, hello and env names left `viso-dsl`.
- [x] Bidirectional link (`viso/src/hot_reload/link.rs`): the link thread connects,
      shakes hands, starts a reader, then writes; host frames reach the loop through a
      bounded queue that wakes it (a full queue stalls the reader, never the loop);
      the loop only `try_send`s into a bounded outgoing queue, and drops are counted and
      sent as `Dropped{count}`. An undecodable frame is NACKed and the channel stays in
      step. The CLI answers the handshake and relays reports, logs, drops, ACKs and
      NACKs.
- [x] Unit tests (§61): every message round-trips (all stages and targets), each bound
      exceeded, a frame over the cap refused unread, truncation at every byte,
      corruption of every byte never panics, another version named, a reserved
      domain named, revision ordering, mismatched token/protocol/session/build on
      both sides (wire, CLI over loopback, facade link and session against a fake
      host: chained commits, NACKs keep last-good, a malformed frame then a good patch).
- [x] Watcher fix found while verifying: the first read at tracking start waited no
      settle window and could read a save in flight half written (the session tests
      failed 6/15 runs at HEAD); it now settles like any other read (0/30).

### H1.3 — Host dev session (`tools/cli/src/dev/`, §4, §6, §7)

- [x] `DevSession` owning the session identity (`DevSessionId`, `BuildId`,
      `ProjectFingerprint`, schema), the session lock (`LockKind::DevSession`, taken
      before the build), the watcher, the source graph and compiler, and the runtime
      connection for the lifetime of `viso run` (`dev/mod.rs`); connection threads in
      `dev/link.rs` with a writer thread per runtime, so the session never blocks on
      a slow app.
- [x] Project watcher on the host (`dev/watch/`): the kqueue / inotify /
      `ReadDirectoryChangesW` backends moved out of the facade (one copy, the app no
      longer watches anything), started before the build; recursive over the scope
      (`*.vs`, `i18n/*.toml`, root `Viso.toml`; shader/asset/Rust sources join with
      their domains), a directory event rescans it so new files and directories
      join; excludes root `target/` `dist/`, hidden dirs, `CACHEDIR.TAG` dirs,
      `node_modules/`, editor temporaries; canonical root; stable FNV-1a
      `source_hash` dedupe. `Viso.toml` edits warn (restart to apply).
- [x] Change coalescer: quiet 5 ms after the latest change, at most 50 ms after the
      first. Measured (`multi_file_save_spread`, release, kqueue): 16 files written
      back to back arrive within 0.95 ms (max), a single save first arrives at
      ~6 ms; recorded in §7.1. One patch in flight; edits saved meanwhile form the
      next batch.
- [x] Incremental compile: one `IncrementalParse` per file, the edit from the
      common prefix/suffix, fed to the new `compile_parsed_for` /
      `plan_view_parsed`; measured on a 36 KB file: full parse 1.71 ms, incremental
      0.048 ms (always in place), whole host compile 23 ms — resolve/typecheck stay
      whole-file (semantic incrementality is later work, recorded in §4). Only the
      edited files recompile; a catalog change rechecks its package's views.
      Cross-file `.vs` imports: not applicable while `view!` compiles each file
      alone (no affected set beyond the file); revisit when the build compiles
      packages.
- [x] Mount inventory: `RuntimeMessage::Mounts([MountEntry{file, path, module
      identity, catalog, capabilities, source_hash}])` once per newly mounted file;
      the host compiles with exactly the build's profile (the mount's grants and
      catalogs — the app's own reload used to drop the grants), and knows which
      version the runtime runs by hash, so an edit saved during the build is the
      first patch and the app sends no source. Keys (`SymbolId`s, `NodeKey`s) are
      not sent: the host derives them from the same compile (needed from H1.4).
- [x] `schema_fingerprint`: `viso_dsl::hotreload::schema_fingerprint()` (compiler
      version, plan format, standard native library signatures), embedded by `view!`
      in the mount record; a mismatch is `Reject::Schema`.
- [x] Protocol 3: `ui` section as the transitional `UiSources` (host-accepted view
      sources + the catalogs the runtime lacks); `HostMessage::Failure{file, lines}`
      (empty clears); ACK per-file `CommitCounts` and span'd notices;
      `NACK_UNKNOWN_FILE`. The app plans every view of a patch before committing
      any (NACK, nothing changed, on any failure).
- [x] `ReloadEvent` and its codec left `viso-dsl` and `RuntimeMessage::InAppReload`
      is gone: the host builds each file's report from its compile and the ACK/NACK
      (`dev/report.rs`), diagnostics resolved on the host; one `dev` event per file
      of a candidate revision (rejected candidates consume a revision number).
- [x] §51 stage names on every `dev` event: host failures by code range (`parse` …
      `shader-compile`), NACK stages (`transport`, `runtime-stage`), commits
      `runtime-commit`; `watch` and `patch-plan` have no failing path yet.
- [x] Without `viso run` the app has no session: its mounts are dropped (§2). The
      facade lost its watcher, `libc` and the Windows file-system features.
- [x] Tests: wire (inventory, `ui`, failure, schema reject, bounds, once-per-domain);
      app session against a fake host (inventory, commit with counts, handler edit,
      host failure shown/cleared, plan failure NACKed then next applies, unknown
      file, state retype, without `viso run`) — deterministic, 0.04 s instead of
      watcher-timed; host session (running content not sent, edit during build,
      one in flight, rejected/reverted, NACK not resent, closed runtime); watcher
      (start delivery, atomic save, created files/dirs, excludes); sources
      (incremental parse equals fresh, catalog recheck).
- [x] Measured app side of the round trip (`patch_to_pixels`, release): transport
      0.029 ms, plan + commit + repaint 1.27 ms median (§48).

### H1.4 — Typed semantic patch (§9–§12)

- [x] Host computes the patch from last-good IR vs candidate IR per mount:
      `viso_dsl::hotreload::patch` lowers diff + migrate + retype to a typed
      `ReloadPlan` (static node index / region-arm-item, `StateKey` + action + initial +
      slots + `Retyping`, `@migrate` chunk) beside the candidate's release-form
      `ViewPackage` (verified behavior bytecode, catalogs compiled in); wire v4
      `UiPatch`. The host finds the build's version by `source_hash` among the versions
      it read and compiles it as last-good; an ACK makes the candidate last-good; an
      unknown last-good sends `ReloadPlan::fresh`.
- [x] Runtime apply engine in `viso-view::dev::commit`: decode → load + verify the
      module at stage (`NACK_UNLOADABLE_VIEW`) → commit at the frame boundary, keyed only
      by static index and `StateKey`; `commit.rs` left `viso-dsl` (the in-process
      `transact` drives the same engine); no source parse, no name lookup in the app.
- [x] Property patch dirties exactly the changed property's `DirtyClass` on its node:
      in-place restyle, rebind marks only newly bound edges, flush only cells whose
      value changed, remount seeds the shown values (a width edit = that node's
      `MEASURE|LAYOUT|PAINT`, a label edit = one reshape). `set_fixed_size` now marks
      `MEASURE` so the parent re-places a resized child (it never moved before).
- [x] The in-app compile path is removed: the facade no longer depends on `viso-dsl`;
      mount records carry `source_hash` and the static shape, not source text;
      `check-release-absence` asserts no `viso_dsl` symbol in the dev artifact.
- [x] `--no-hot-reload`: dev artifact, session up, candidates compiled and reported,
      nothing sent (`dev{outcome:"held"}`) (§2.1).
- [x] Integration tests (§62 UI, Invalid `.vs`) against a fake host over the real dev
      channel: one-property edit → same process/tree, exact dirty mask, state kept;
      height edit → state, focus and scroll offset kept; host-rejected source →
      revision 1 still running, no node changed; unloadable patch NACKed, next applies;
      out-of-order patch → `NACK_REVISION_MISMATCH`; window opened after a patch mounts
      the patched view. Host side: patch equals the planner's, chained last-good, fresh
      plan for an unread build, held without hot reload.
- [x] Re-measured edit-to-pixels (release, `patch_to_pixels`): compile 1.33 ms, plan
      0.002, encode 0.001 (449 B), transport 0.074, commit 0.006, repaint 0.001 —
      1.43 ms median from compile; app side 0.08 ms (was 1.27). Detect is §48's ~6 ms +
      5 ms coalesce.

### H1.5 — Diagnostics and output (§51–§53)

- [x] `ReloadStage`(`viso_view::dev::wire::Stage`) already covers all 15 §51 stages;
      `rejection_stage` maps a host diagnostic's code range to one, a runtime NACK or
      commit carries its own stage directly — no gap to close.
- [x] `DirtyCounts` (`viso-view::dev::wire`, protocol v5): 8 named class counts, computed
      in `commit()` by scanning the view's static nodes against the live `NodeStore`
      after the commit, summed into `CommitCounts.dirty` across every mount of a file.
      Human line rewritten to §53's shape: `✓ {file} patch r{base} -> r{candidate}
      {ms} ms   N node(s) {class}-dirty[, ...]` (omitted when nothing is dirty, e.g. a
      text-only reshape), `✗ {file} candidate       kept r{last_good}   {CODES}`, a held
      candidate as `… {file} candidate       compiled, not applied (--no-hot-reload)`.
- [x] `--json` `dev` payload adds `runtime_session_id` (connected runtime's
      `RuntimeSessionId` hex, empty without one) and a nested `dirty` object; every field
      is a revision/count/stage name/diagnostic code, never user source or secrets (§47).
- [x] Overlay (`viso::hot_reload::overlay`) is already app-side, a detached subtree driven
      by the `Failure` the host sends on a NACK/rejection — no change needed.
- [x] `Viso_CLI.md` §36.4 and `Viso_Hot_Reload.md` §37/§37.1 updated for `dirty` and
      `runtime_session_id`; §1.3/§1.4/§46/§48 already described the host/app split
      accurately. ADR-0015's 2026-10-09 amendment already covers the split; the dirty
      counts are a diagnostics addition, not a new architectural decision.

### Done

- [x] Editing a label, a handler body and a state type in a running desktop app each
      arrive as a typed patch from `viso run` and apply without losing unrelated state;
      a broken edit leaves the last-good UI; the app links no compiler. Proven by
      `hot_reload::tests::a_label_a_handler_and_a_state_type_edit_each_apply_without_losing_state`
      (two mounts, cross-checked against `check-release-absence`).

---

## H2 — Phase B: structural and state patches (§12–§15, §66)

Goal: inserts, removals and reorders keep identity and state; incompatible state
resets only its own scope.

### H2.1 — What exists

- [x] Structural diff aligning each parent's children by type, node name and kind; a
      region aligns only with one of the same form and arm count.
- [x] Commit scoped to the view's subtree: rebuild in place under the same parent and
      sibling position; only the view's static edges and region hooks replaced.
- [x] Focus, scroll (restored after first layout), text edit buffer and selection, and
      in-flight transitions migrate by `NodeKey`; region-mounted nodes by item key
      path; lost focus/scroll reported.
- [x] State migration by `SymbolId` for UI cells and VM slots through one plan;
      conversion matrix, record extension with defaults, surviving enum variant,
      `@migrate(from:)`, `E5101` reset notice, `E5102` collision rejection.
- [x] Tasks cancelled with the old code; effects cleaned up and remounted; resources
      keep state and reload.

### H2.2 — Remaining

- [x] Structural patch as node-level `StructuralOp::{Remove,Replace,Insert}` on the
      kept tree instead of freeing and rebuilding the view's root: kept nodes stay
      the same `NodeId`s, so nothing needs carrying for them (§12 structural insert).
- [x] `PATCH_WITH_SCOPED_RESET` scoped to the narrowest owner: every state a commit
      resets (new, or kept but incompatible) is reported by its own `SymbolId`/
      `StateKey`, not only as a count — `CommitReport::state_resets`, bridged to
      `HotReloadReport::state_resets` and the ACK's `PatchAck::state_resets` (§8).
- [x] State identity across renames: an explicit `@stable("id")` on a state or
      component keeps its `SymbolId` across a rename (DSL §88), tested; a plain
      rename still resets. Two decls of the same kind claiming the same
      `@stable` id is `E5104`.
- [x] Active tab and navigation state contracts (§15): `MigratableState::ACTIVE_CHILD`;
      `Tabs` and the newly schema-registered `NavigationStack` marked migratable with
      it; `commit.rs` carries which immediate child had `hidden` clear across a
      Replace boundary, tested.
- [ ] Composition (IME) state carried with the edit buffer when compatible (§15).
- [ ] Component instances inlined from another file reload when that file changes
      (cross-file dependents found from the module graph).
- [ ] Unit tests (§61): scoped reset planning, SymbolId preservation across
      reorder/insert, compatibility matrix rows.
- [ ] Integration: insert before a focused, scrolled, edited node in a running app;
      assert same `NodeId`, focus, offset, selection; reorder keyed siblings.

### Done

- [ ] Inserting, removing and reordering nodes in a running app keeps every unrelated
      node's identity and state; an incompatible state change resets only its owner.

---

## H3 — Phase C: game systems (§16–§18, §67)

Goal: a game `.vs` edit swaps logic at a fixed-tick boundary with the world kept.

### H3.1 — What exists

- [x] `hotreload::game::classify`: Unchanged / Presentation / Logic / Logic with state
      migration / World Rebuild from the stable-id diff, `E5103` with the deciding
      change.
- [x] `Scheduler::reload` at a tick boundary keeping world, rng, tick, clock, timers
      and held input, carrying Simulation and `@local` states by stable id and
      schema; smoke tick on a copy; `rebuild` in a shadow game with
      `Rebuild::KeepCharacters`.
- [x] `hotreload::game::swap` applying a tier, last-good kept on any failure.

### H3.2 — Remaining

- [ ] Game host wiring: the host classifies the candidate and sends `SystemPatch`
      (bytecode + tier + state plan); the runtime stages it and commits after the
      current tick completes, never mid-tick (§16, B.4).
- [ ] Integration test (§62 Game): run tick N, stage a patch during N, assert old code
      completes N, new code starts N+1, world and entity ids identical.
- [ ] Replay timeline: a live patch is recorded as `PatchRevision applied at TickId`
      with the system code revision and any state resets; replay re-applies it at the
      same tick; deterministic-replay mode refuses live patches with a stage
      `runtime-stage` NACK (§18).
- [ ] Audio host swap on a logic reload without a gap (lock-free slot hand-off; with
      the D5 audio item).
- [ ] `viso game peek` / `viso game record` over the dev channel against the running
      session (CLI §22.3).

### Done

- [ ] Editing a system's movement speed in a running game changes it at the next tick
      with the world, entities and score kept; a broken edit keeps the last good.

---

## H4 — Phase C: assets and fonts (§22–§23, §67)

Goal: an asset change swaps one resource revision and dirties only its dependents.

- [ ] Watch `assets/**` (and asset entries named by `Viso.toml`) on the host.
- [ ] `ResourcePatch`: content hash → decode/build candidate on the host (images,
      SVG) → `ResourceRevision++` → logical `ResourceId` remapped on the runtime only
      after the new payload is ready (§22); old payload retired after the frame that
      stopped using it.
- [ ] Precise dependents: only nodes and paint ranges referencing the resource get
      `PAINT` (or `MEASURE` when intrinsic size changed); no global cache clear.
- [ ] A failing decode is a `resource-build` NACK, the old resource stays.
- [ ] Tests: image edit repaints only its nodes; size change remeasures its node; a
      corrupt file keeps the old image.
- [ ] Font patch (§23): `FontFaceRevision` bump from a dev font file change, reflowing
      only paragraphs using the face — **waits for the font work in progress**; it
      reuses `text/src/progressive.rs::FontRevision`, no change to font code from
      this plan until then.

### Done

- [ ] Replacing an image in `assets/` updates it in the running app with only its
      dependents dirty; a corrupt image keeps the last good.

---

## H5 — Phase C: shaders (§19–§21, §67)

Goal: a shader edit compiles in the background, swaps at a GPU-safe frame boundary,
and a failure keeps the drawn pipeline.

### H5.1 — What exists

- [x] `ProgramReload`: worker-thread compile, newest submission wins, `poll()` between
      frames returns a swap with instance/uniform re-encode, failure keeps the live
      pipeline (`shader/src/program/reload.rs`); backend rejection keeps the pipeline
      on Metal (`viso/tests/shader_reload.rs`).
- [x] Fence-based deferred destruction primitive (`gpu/src/retire.rs`).

### H5.2 — Remaining

- [ ] Host compiles shader candidates (`.vs` shader declarations, and external shader
      files in the watch scope) through Shader IR to every backend the runtime's
      target uses and validates them; `ShaderPatch` carries the backend code and the
      interface (§19, §43 target-specific validation).
- [ ] Runtime creates the candidate pipeline off the frame, swaps it at the GPU-safe
      frame boundary, and hands the old pipeline/resources to the retire queue so
      in-flight frames finish with them (§21).
- [ ] Interface change: dependent material/render schemas validated and their
      bindings/instance buffers rebuilt before a scoped commit; an incompatible
      host-side GPU ABI is a `gpu-validate` NACK, not a crash (§20).
- [ ] Source-keyed pipeline cache: an edit that leaves generated code unchanged reuses
      the existing pipeline; a bounded cache evicts pipelines no revision references
      (makepad leaks here).
- [ ] Cross-domain candidate: a save touching a view and its shader commits both or
      NACKs both (§40).
- [ ] Tests (§62 Shader): valid pipeline, invalid candidate → old pipeline drawn;
      interface change re-encodes instances; retired pipeline destroyed only after
      its fence; pipeline count stays bounded over 1000 edits.

### Done

- [ ] Editing a shader in a running app swaps it at a frame boundary; a syntax error
      never blanks the screen.

---

## H6 — Phase D: Rust warm restart and DevSnapshot (§24–§32, §68)

Goal: a Rust edit rebuilds while the old app keeps running, then the app restarts with
its compatible state restored.

### H6.1 — Build coordinator

- [ ] Host watches Rust sources in the active workspace dependency closure and build
      scripts/config that affect the artifact (§6), from `cargo metadata`.
- [ ] `RustBuildService`: incremental `cargo build` of the dev artifact in the
      background; the old app is never stopped before success (§25, B.5); a failed
      build reports diagnostics (`warm-restart` stage) and keeps the old app.
- [ ] A `.vs`-only change never triggers a Rust build (B.3); a Rust change that only
      touches `ui!`/`component!` still needs the build.
- [ ] Coalesce: edits during a build restart the build; the newest successful build
      wins.

### H6.2 — DevSnapshot (§29–§32)

- [ ] `DevSnapshot { snapshot_version, source_build, app_schema, component_state,
      system_state, ui_ephemeral, app_extensions }` in ende binary, typed by
      `SymbolId` + schema fingerprint; never pointers, handles, fds, sockets, task
      stacks or platform objects (§30).
- [ ] `DevSnapshot` and its endpoint join the release-absence markers
      (`xtask/src/absence.rs`).
- [ ] Capture: every mounted view's state cells and VM slots, focus identity, scroll
      offsets, text editing logical state, navigation/tab state, window geometry,
      game snapshots when the Game Profile exposes them (`game/snapshot.rs`), and app
      `DevStateExtension`s registered explicitly.
- [ ] Restore through the same compatibility matrix and `@migrate` as a patch;
      incompatible entries reset with `E5101`; a restore failure is a
      `snapshot-restore` NACK and the app starts fresh.
- [ ] Storage: host memory, current session only; opt-in disk cache under the
      ignored dev cache with sensitive-field exclusion; never printed in logs or JSON
      (§32, §47).

### H6.3 — Desktop warm restart (§26)

- [ ] Sequence: build success → request snapshot → stop old process → launch new →
      handshake → restore → resume; window geometry restored from the snapshot.
- [ ] `WARM_RESTART_REQUIRED` class reported by the host when native schema, Rust
      types used by the runtime, platform bindings or link-time features change (§8).
- [ ] Integration test (§62 Rust): app with state, edit Rust, assert old app runs
      until the build succeeds, warm restart, assert state restored; a failing build
      leaves the old app running.
- [ ] Measurements: Rust incremental build time, snapshot time, relaunch time,
      restore time (§48).

### Done

- [ ] Editing Rust code in a running desktop app rebuilds in the background, then
      restarts it with its counters, text fields, focus and window geometry back.

---

## H7 — Phase E: Android emulator, iOS simulator, web (§5, §27, §28, §63, §69)

Goal: the same patches reach emulator, simulator and browser; only transport and deploy
differ.

- [ ] `viso run ios` / `viso run android` / `viso run web-*` targets and `--device`
      in the CLI (`Viso_CLI.md` §3, §2).
- [ ] Android emulator: profile → adb serial mapping, `adb reverse` for the dev port
      (no LAN exposure), dev APK build/install/launch, warm restart delays stopping
      the old process until the candidate is built (§27).
- [ ] iOS simulator: `simctl` install/launch, host-reachable loopback transport, no
      signing or provisioning (§28).
- [ ] Web: dev server with a WebSocket dev channel bound to loopback (LAN opt-in),
      the web dev runtime only in the dev build, patches applied in the browser;
      Rust/wasm change rebuilds and restores from a snapshot.
- [ ] The app never watches files on mobile/web (the in-app watcher is already gone
      after H1).
- [ ] CI: Android fake + emulator job and iOS simulator job covering boot, install,
      connect, hot patch, Rust warm restart, reconnect (§63).

### Done

- [ ] A `.vs` edit patches a running Android emulator, iOS simulator and browser app;
      a Rust edit restarts each with state restored.

---

## H8 — Phase F: multiple runtimes (§43–§44, §52, §70)

Goal: one source graph serves several running runtimes with per-runtime revisions.

- [ ] Connection manager holding several runtimes; shared parse/module graph/type
      check, target-specific shader validation, capabilities and resources.
- [ ] Per-runtime revision and status; a target whose shader fails stays on its last
      good while others advance (§44).
- [ ] Status surface for Studio (§52): running/candidate/last-good revision, target,
      patch class, scoped resets, compile time, transport bytes, commit time, Rust
      rebuild state, connection state — as a stable JSON stream.

### Done

- [ ] Desktop and web runtimes of one project receive one edit; a target-specific
      failure leaves only that runtime on its last good, shown per runtime.

---

## H9 — Acceptance (§61–§64, §71)

- [ ] §61 unit suite: semantic diff, SymbolId preservation, compatibility matrix,
      revision ordering, PatchBundle bounds, ACK/NACK, scoped reset planning,
      snapshot schema.
- [ ] §62 integration suite under a test host driving a real dev app: UI, invalid
      `.vs`, shader, game, Rust.
- [ ] §63 simulator/emulator suite.
- [ ] §64 release absence suite in CI.
- [ ] Fuzz: random and mutated dev frames never panic the runtime decoder and never
      change last-good.
- [ ] §71 Definition of Done checklist test, one test per item.
