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
- [x] Composition (IME) state carried with the edit buffer when compatible (§15):
      confirmed already correct as a side effect of the existing `SELECTION` carry
      (`Buffer` bundles composition with text/selection); regression test added.
- [ ] Component instances inlined from another file reload when that file changes
      (cross-file dependents found from the module graph). Deferred: needs real
      multi-file module resolution in the hot-reload compile pipeline (`compile_unit`
      builds a `ModuleGraph` from exactly one `SourceUnit`, `crates/dsl/src/frontend.rs`)
      — an unrelated architectural capability from this slice's node-identity/patch
      mechanics (AGENTS §40). A future slice's own item.
- [x] Unit tests (§61): scoped reset planning (`commit.rs`'s `migrate_states` tests,
      `state_retype.rs`), SymbolId preservation across reorder/insert (`diff.rs`'s
      `a_node_under_a_replaced_ancestor_is_not_its_own_insert_or_remove`,
      `a_replaced_container_keeps_its_children`,
      `swapped_siblings_keep_one_and_rebuild_the_other`; `migrate.rs`'s
      `a_kept_node_with_migratable_state_carries_it`), compatibility matrix rows
      (`state_retype.rs`).
- [x] Integration: insert before a focused, scrolled, edited node in a running app;
      assert same `NodeId`, focus, offset, selection
      (`focus_and_scroll_follow_their_nodes_past_an_inserted_sibling`,
      `a_kept_text_input_keeps_its_identity_and_buffer_past_an_inserted_sibling`);
      reorder keyed siblings (`diff.rs`, the Done test's own reorder step).

### Done

- [x] Inserting, removing and reordering nodes in a running app keeps every unrelated
      node's identity and state; an incompatible state change resets only its owner.
      `insert_remove_reorder_and_a_scoped_reset_each_apply_without_disturbing_the_rest`
      (`crates/viso/src/hot_reload/mod.rs`) is the composite acceptance test: a
      two-mount session, insert/remove/rename/reorder on one mount, the other mount
      and its unrelated state untouched throughout, and an incompatible retype
      resets only its own `StateKey`.

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

No game runs in an app yet: `view!` compiles a file's systems into its module, but
nothing builds a `Scheduler` for them outside tests and `viso test game`. The running
session comes first, then every reload item builds on it.

- [x] Game session: a `view!` (or `component!` / package / dev-plan) mount whose
      module declares systems starts a `viso_view::GameSession` owned by the mount's
      root — its own VM over the shared module, `Natives::standard`, the module's
      capabilities — registered as a frame client of the window's store
      (`viso_ui::FrameClient`); the facade advances every client each frame after the
      reload commit (the ticks the clock owes, then `FrameUpdate`), feeds it the
      window's keys while no UI node holds keyboard focus and releases its input when
      the window or the UI takes focus; freeing the root drops it, a reload that
      rebuilds the root moves it (`game_session.rs`, store and facade loop; the
      headless backend gained a frame budget for apps that never stop animating).
- [x] Wire: a `system` section — `SystemPatch { games: [GamePatch { file, tier }] }`,
      `GameTier` Presentation / Logic / LogicMigration / WorldRebuild { keep } —
      encoded and decoded with the section bounds; the candidate module is the
      behavior of the file's view in the same patch's `ui` section, never sent twice
      (dev protocol 8).
- [x] Host: a candidate keeps the behavior program of a module declaring systems
      (`CandidatePlan::game`), so each mount's last good holds it; a candidate of a
      game file is classified against it (`hotreload::game_patch`), reported as the
      `E5103` note (its code in the `dev` event) and sent as the file's `ui` and
      `system` entries of one candidate revision; `Unchanged`, a game the mounts do
      not run yet and a view without one send no `system` entry; an unknown last
      good rebuilds the World keeping its characters (the host's World Rebuild
      policy); an app that does not advertise `system` gets the view alone with a
      warning; the ACK makes the candidate the last good.
- [x] Runtime stage: the runtime advertises `system`; a `system` entry whose file has
      no view with systems in the `ui` section refuses the patch, and the candidate
      module links into a VM at stage time with nothing live touched
      (`NACK_UNLOADABLE_SYSTEM`, stage `runtime-stage`); a mount whose game did not
      start gets one with its view instead.
- [x] Runtime commit at the tick boundary the frame is at, before its ticks run:
      systems first — every session of the file proves its swap
      (`Scheduler::prepare_reload` / `prepare_rebuild`, the smoke tick run, the old
      build held) — then the views, then every proved swap commits; a fault puts every
      proved swap back and refuses the whole revision (`NACK_SYSTEM_FAULT`, stage
      `runtime-commit`, the fault as an error log line naming the system); the ACK
      carries a `GameCommit` per file — tier, sessions, the first tick of the new
      build, the carried and reset states and the kept characters — shown in the
      `dev` event's `game` object and the human line.
- [x] Game faults reach the developer: a session reports each new fault once (a
      system faulting every tick the same way is not repeated) to a bounded queue,
      `take_game_faults`, with the starts that failed; the facade forwards them to
      `viso run` as log lines, or prints them without it.
- [x] Integration test (§62 Game): run tick N, the patch sent while it runs; the old
      code completes N, the new code runs N+1, the World, its entity ids and the
      session identical (`hot_reload::tests::games`, with the faulting, World Rebuild
      and missing-view cases).
- [x] Replay timeline: a recording tape records each live reload as a `TapePatch` —
      the tick it applied before, the dev revision, the build hash, the build's module
      and the states it reset — with the module the run started on; the binary form
      carries them (the text form lists them as comments); a replay checks the start
      build, links every patch up front through the game's `Linker` and swaps each in
      before its tick; a rewind folds the later patches into one at the rewound tick;
      a game replaying a tape refuses live patches (`NACK_REPLAYING`, stage
      `runtime-stage`, with a warning line) (§18).
- [x] Audio: a session whose module has audio systems starts them on the device the
      app installed (`install_game_audio`; the facade installs `GameAudio` on the
      default output, the device opened only for a module with audio systems), also
      when a reload adds them; a reload swaps them without a gap: the new
      `AudioProgram` — VM, instances, its own block and message slot — is built and
      warmed off the audio thread and published through the host's `AudioSwap` (two
      atomic pointers), installed before the next block, the old one handed back and
      dropped on the game's thread once a frame; the queues and status stay with the
      host and the event queue moves to the new block, so the link stays valid; the
      install and the new program's first block allocate nothing; audio faults are
      reported once each with the game's faults.
- [x] `viso game peek` / `viso game record` over the dev channel against the running
      session (CLI §22.3): a loopback control endpoint of `viso run`, named with its
      token in the session lock (`LockGuard::publish`), relays a query to the runtime
      (`HostMessage::Game`, `RuntimeMessage::Game`, dev protocol 9) and prints or
      writes the answer; the game is named by its `.vs` file; a peek reads any state,
      written as JSON by its type from the file's last good (every system state now
      carries its shape); a recording restarts the World with `Rebuild::Fresh` and
      answers with the tape once the game ran the ticks asked for, written as text, or
      binary when it took live patches; an answer the app never sends is refused when
      it exits.
- [x] Docs: Hot Reload §16–§18 and §34–§37 (the session's tick-boundary commit, the
      `system` section, the game query messages, the NACKs and ACK fields, the replay
      timeline, protocol 9), CLI §22.3, §36.4 (the `dev` event's `game` object) and
      §44 (the lock's endpoint line), DSL §110 (the two-phase swap, `GameSession`,
      the audio swap, tape patches, `SystemLayout::states`).
- [ ] Not in this phase: `GameViewport` and the 3D lane that draws the World (the
      Rendering spec's Mesh/3D lane needs its design and ADR first), HUD reads of game
      state (§109 Reactive Query) — tracked in `todo_dsl.md` D5.

### Done

- [x] Editing a system's movement speed in a running game changes it at the next tick
      with the world, entities and score kept; a broken edit keeps the last good
      (`hot_reload::tests::games::a_speed_edit_applies_next_tick_and_a_broken_one_keeps_the_last_good`;
      an edit that does not compile never leaves the host, its failure shown over the
      last good).

---

## H4 — Phase C: assets and fonts (§22–§23, §67)

Goal: an asset change swaps one resource revision and dirties only its dependents.

There is no asset system yet (no `.vs` way to name an asset, no decoder but the
emoji PNG path, no registry, no `Image` widget), so H4 builds it first, then its
reload. makepad has no asset reload at all (its watcher reloads only `script_mod!`
blocks; images, SVGs and fonts are read once, cached by path, and change only after a
restart); avoid what it stacks instead: three path-keyed caches with no shared
invalidation, encoded bytes and decoded pixels kept alive beside the texture,
swallowed decode errors, GPU memory freed only on slot reuse, fonts memory-mapped from
editable source files.

### H4.1 — Asset system

- [x] Decode (`render/src/asset.rs`): PNG and JPEG (`zune-png`, `zune-jpeg`) to
      premultiplied RGBA8 in the texture convention (`RasterImage { width, height,
      pixels }`), the format sniffed from the bytes (`AssetFormat::sniff`); SVG to a
      `VectorScene { paths, size }` (`SvgScene::into_vector`) that `place`s its paths
      stretched into a box, strokes and dashes scaled, colors optionally tinted.
      Errors are values (`AssetError`); `MAX_DIMENSION` / `MAX_PIXELS` are checked
      from the header before any pixel is allocated.
- [x] Identity: `ResourceId::of` (FNV-1a of the path under `assets/` as
      `normalize_path` spells it — absolute paths and `..` refused),
      `ResourceRevision` per payload; paths exist only while compiling and packaging.
- [x] UI (`ui/src/asset.rs`): each `NodeStore` keeps an `AssetTable` — a resource is
      offered (encoded bytes, by revision; the same or older bytes again are free),
      decoded (`provide_asset`, the bytes dropped), and for a raster resident
      (`attach_texture`, the texture it replaces handed back to release; a texture of
      a revision the table moved past handed back unused). `Content::Asset {
      resource, draw, natural, fit, tint }` carries what it draws, so paint reads no
      table; `set_asset(node, id, fit, tint)` shows a resource; `AssetFit::{Contain,
      Cover, Fill}` (a cover crops a raster in the texture and clips a vector to the
      box). A raster keeps drawing its resident pixels until the new ones are
      attached; then only the nodes showing it are rewritten, `PAINT` when the
      natural size holds, `MEASURE | LAYOUT | PAINT` when it moves.
- [x] DSL: `asset("images/logo.png")` takes a string literal, checks the file exists
      under the package's `assets/` (`AssetCatalog`, scanned once, formats sniffed
      from each file's head; the `view!`/`component!` profile loads it) and is an
      `ImageAsset` (PNG, JPEG) or an `SvgAsset` (SVG) — prelude records only `asset`
      makes, a constant the behavior builds, which states and branches hold; the
      compile lists every asset it names (`Compiled::assets`). `Image` and `Svg` leaf
      widgets take `source`, `fit: ImageFit` and `tint: Option<Color>` (`source`
      dirties `MEASURE | LAYOUT | PAINT`, `fit` and `tint` `PAINT`). `E3713` for a
      wrong argument count, a non-literal path, a path outside `assets/`, a missing
      file (with the nearest path as a fix), a file in no asset format, a compile
      with no asset directory and a record literal of either; DSL spec §U5.1.
- [x] View and package: `ControlKind::Asset` (`Image`, `Svg`) reads `source`, `fit`
      and `tint` as entries and shows them with `set_asset`; the host holds each
      asset's bytes (`AssetBytes`: embedded or shared) and a node offers them to its
      store the first time it shows the resource, so only what shows decodes; a
      reload keeps the host's assets, the new host's own taking over. `view!` and
      `component!` embed the bytes of every asset the view names with
      `include_bytes!` (cargo rebuilds on change); `ViewPackage::assets` carries
      them (`build_view_package_for` reads them from the profile's asset directory,
      `E3713` when one cannot be read; ascending ids, checked on decode).
- [x] Facade (`viso/src/assets.rs`): before each frame's layout the app serves
      every window whose table moved — a payload it already decoded is provided at
      once, anything else is decoded on the `viso-assets` worker (which wakes the
      loop), once for every window; each window uploads its unresident rasters
      (`Rgba8Unorm`, premultiplied) into its own device and releases what they
      replace through the renderer. The app keeps payloads weakly and a table drops
      a raster's pixels once its texture is attached, so a resident image costs only
      its texture; textures live on the device across a renderer rebuild and a
      device loss. A payload that does not decode is reported once and never
      retried at that revision; the last good keeps drawing. Counters: decodes,
      decode failures, uploads, upload bytes (`VISO_DEBUG`). `examples/images`.
- [x] Rust: `viso::asset!("…")` checks the path like `asset("…")` (`E3713`) and
      embeds the file as a `viso_ui::Asset`; `widgets::image_asset` shows one
      (`BuildCx::asset` offers its bytes), fitted, tinted and labelled. `Role::Image`
      (AccessKit `Image`, ARIA `img`, UIKit's image trait) names an image by its
      label; an unlabelled one is decoration. The DSL's `semantics.*` reaching the
      node is `todo_dsl.md` D2.7.

### H4.2 — Asset reload

- [x] Watch: every file under an `assets/` directory joins the watch scope; the
      watcher reads bytes and hashes them (`content_hash`), an unchanged hash sends
      nothing, a `.vs` or catalog that is not UTF-8 is skipped. The mount report
      (`MountEntry::assets`, from `view!`'s record) names the package's asset
      directory and the resources the build embedded for the file; the host keeps
      each directory's files, formats and catalog, compiles candidates against it,
      and rechecks a view that did not compile once an asset appears.
- [x] Wire and host: `PatchSection::Resource(ResourcePatch { assets: [ResourceAsset
      { path, hash, len }] })` on `Domain::Resource`; the bytes travel ahead as
      `HostMessage::Blob` chunks (`BLOB_CHUNK` each, `MAX_ASSET` per asset, sent once
      per content), assembled by hash and claimed by the patch (`Blobs`: a blob cut
      short, out of order or of other bytes is not claimed); `PatchAck::resources`
      (`ResourceCommit { id, nodes, dirty }`); `NACK_UNLOADABLE_ASSET`; protocol
      version 10. The host sends an asset the app holds (a mount embeds it or a
      patch carried it) once its content changes, and one a candidate view names
      that the app lacks with that view; it decodes each first with the app's
      decoder (`viso_svg::decode_asset`), and one that does not decode is reported
      (`E3713`, stage `resource-build`, against its path) and not sent.
- [x] Runtime: the reader assembles the blobs and decodes each patch's assets off the
      main thread; a missing or undecodable one refuses the revision at
      `resource-build` with a log line naming its path, the old resource staying.
      The commit, at the frame boundary after the views, records the newest bytes
      app-wide (a window or mount opened later shows them; a node showing a resource
      its host has no bytes for asks for it), provides the payload to every store
      showing it — only those nodes are rewritten — uploads rasters, releases the
      replaced texture through the renderer once no frame in flight draws it, and
      reports each resource's dirty counts.
- [x] Tests: an image edit repaints only its nodes; a size change remeasures its node;
      a corrupt file keeps the old image; an SVG edit; the dirty counts in the ACK.
- [ ] Font patch (§23): `FontFaceRevision` bump from a dev font file change, reflowing
      only paragraphs using the face — **waits for the font work in progress**; it
      reuses `text/src/progressive.rs::FontRevision`, no change to font code from
      this plan until then.

### Done

- [x] Replacing an image in `assets/` updates it in the running app with only its
      dependents dirty; a corrupt image keeps the last good
      (`hot_reload::tests::an_asset_patch_swaps_the_resource_and_dirties_only_the_nodes_showing_it`,
      `dev::tests::an_asset_that_does_not_decode_is_reported_and_kept_from_the_app`;
      in `examples/images` under `viso run`: an SVG fill edit applies with one node
      painted, a `viewBox` edit with one node measured, laid out and painted, a PNG
      swap with one node painted, a truncated PNG is reported `E3713` at
      `resource-build` and nothing is sent). The font patch stays open with the font
      work.

---

## H5 — Phase C: shaders (§19–§21, §67)

Goal: a shader edit compiles in the background, swaps at a GPU-safe frame boundary,
and a failure keeps the drawn pipeline.

### H5.1 — What exists

- [x] `ProgramReload`: worker-thread compile from source, newest submission wins,
      instance re-encode by member name, failure keeps the live pipeline — the
      reduced in-app design, replaced by the patch path below and removed.
- [x] Fence-based deferred destruction primitive (`gpu/src/retire.rs`).

### H5.2 — Remaining

ShaderBrush first: a running app draws no user shader yet (the renderer only knows its
built-in pipelines, `view!` drops the checked shaders), so the app-side path of
Rendering §27.2 lands before the reload items, end to end, then the reload items build
on it.

- [x] GPU: user programs on the RHI trait — `GpuBackend::{Compiler, program_compiler,
      install_program}`: a `Send` compiler builds a backend program off the render
      thread from code in the backend's language, install registers it as a pipeline;
      a program's uniform block may come inline on every backend, and
      `Caps::program_instance_align` is the instance-offset alignment its storage
      buffer needs. Portable binding limits `PROGRAM_TEXTURES` (4) /
      `PROGRAM_SAMPLERS` (2), samplers numbered after the textures where one binding
      space holds both; D3D12 gives a program its own root signature (root SRV for
      the instances), WebGPU its own group 0 (cached per instance range) and a
      program view of each bind group. `ProgramReload` hands the new pipeline to the
      caller to install. Metal and Vulkan (MoltenVK) runtime-verified; D3D12 and
      WebGPU type-checked.
- [x] Headless: a program carries a `CpuProgram` the shader crate supplies —
      `viso_shader::program::CpuShader`, the reference interpreter reading the draw's
      uniform and instance bytes through the program's layout and drawing into the
      framebuffer under the scissor (float targets keep the floats) — so the headless
      backend and its goldens draw user shaders; a draw hands the program its bind
      group's textures and samplers in slot order (`CpuDraw::{textures, samplers}`,
      `CpuTexture`), and `HeadlessRaster::read_texture` reads a target back as a GPU
      backend does. `shader_golden` draws every scene headless too, on every
      platform.
- [x] Shader: the brush profile (`shader/src/program/brush.rs`) — reserved members
      `uniform viewport_size: Vec2F32`, `instance rect_pos: Vec2F32`, `instance
      rect_size: Vec2F32`; every other instance member is a parameter of a
      host-typed kind (`ParamKind`: `F32 I32 U32 Vec2F32 Vec3F32 ColorLinear`); no
      other uniform, no texture or sampler; each break `E8107`. `BrushLayout`
      (stride, reserved offsets, uniform size, parameter descriptors) encodes a
      node's parameters once into an instance (`encode`), rewrites one in place
      (`set`), places the box (`place`) and writes the uniforms
      (`write_uniforms`). `program::code(program, interface, ShaderLang)` gives each
      backend's code from one program: MSL, WGSL, HLSL, SPIR-V from the WGSL through
      naga (the optional `spirv` feature, enabled only by build-time and dev-host
      tools), the `CpuShader` for headless. `shader_golden` draws the spec example
      as a brush on CPU, headless, Metal and Vulkan from one call each.
- [x] Render: `Primitive::Shader(ShaderDraw { rect, program: ProgramSlot, params:
      Arc<[u8]> })`; programs installed per renderer slot (`set_program`,
      `install_program`, a replace retiring the old pipeline through the fence,
      `release_program`) in `render/src/program.rs`; one persistent instance pool per
      program with runs aligned to `program_instance_align`; batch family `Program`
      whose key carries the slot (bits 50..64), so adjacent draws of one program
      merge; offscreen origin subtraction; never folded into a group opacity;
      `BatchPipeline::Program` in inspect, `FrameStats::{shader_instances,
      program_draws_dropped, programs}`; `render/tests/program_draw.rs` on headless,
      and the same scene on Metal against it.
- [x] UI: `Content::Shader { program: ProgramSlot, params }` paints a node's box as a
      `Primitive::Shader`; `ShaderSlots` (`ui/src/shader.rs`) keeps the store's
      `Arc<BrushProgram>`s by slot, found by identity, with a revision the facade
      installs from;
      `NodeStore::{set_shader, set_shader_params}`, a
      parameter change or the same program with new parameters `PAINT` only. Render
      gains `BrushProgram` (name, layout, code per language) and
      `Renderer::install_brush`, and re-exports the brush types.
- [x] DSL: a shader name heads a record literal of its parameters
      (`Ripple { tint: c, rings: 8.0 }`), typed against them — every instance
      member but the box, `F32 I32 U32` as themselves, `Vec2F32`/`Vec3F32` as the
      `viso::math` values, `ColorLinear` as `Color`, none defaulted; a `ShaderView`
      leaf widget whose `shader` property takes one (`E2103` otherwise), dirtying
      `PAINT` only; `CheckedShader::brush` carries the `BrushLayout`, and the
      brush profile's breaks (`E8107`) are reported once per shader, only when a
      view paints with it, pointing at the first such view
      (`dsl/tests/shader_brush.rs`); DSL §100 and Rendering §27.2 updated.
- [x] Package codec: `PackedBrush { name, layout, code: PackedCode }`
      (`shader/src/program/package.rs`) — a brush built for one backend at package
      time: MSL/WGSL/HLSL text, SPIR-V bytes, or the program itself for the
      reference interpreter; ende codecs for `Program`, `BrushLayout` and
      `PackedBrush`, nesting bounded, a decoded CPU program revalidated and its
      layout required to be its own; `unpack` gives the `ProgramCode` without
      generating anything.
- [x] View and package: a view's shader table lists each shader its `ShaderView`s
      paint with once (`ViewBehavior::shaders`, resolved per paint site by component
      and span, `Compiled::paints`); `ControlKind::Shader` names its program by table
      index (`Control::shader`) and delivers the value's fields encoded by the
      brush's layout (`ColorLinear` from `Color` in linear light), repainting when
      the parameters or the program change. `PackedBrush` carries code per
      language; the release package packs `TargetProfile::shader_langs`
      (`ViewPackage::shaders`, `ViewShader`), `view!` packs every backend's and
      keeps the target's through `viso_gpu::__per_backend!` (same `cfg`s as
      `Backend`) beside the reference interpreter's program, the dev host packs the
      language the runtime reports (`RuntimeHello::shader_lang`, protocol version
      11) into the view patch, an in-process reload the interpreter's; `viso-dsl`
      always builds SPIR-V. The same bytes load as one `Arc<BrushProgram>` per
      thread (`load_shader`, weak cache), so one shader holds one slot and one
      pipeline per window; a slot counts the nodes painting with it and empties
      with the last (no sweep); the facade installs the slots that changed after
      the settle and releases the emptied (`viso/src/shaders.rs`), a refused
      program reported once. `content_hash` moved to `viso-ende`. Tests:
      `dsl/tests/view_shaders.rs`, `viso/tests/dsl_shaders.rs`,
      `viso::shaders::tests`, `ui::shader::tests`.
- [x] Example `10-custom-shader` and an end-to-end test: a `.vs` `ShaderView` drawn
      headless against the reference interpreter and on Metal — a `Ripple` brush
      whose rings a slider drives through a state; the headless picture is checked
      at the box's center, edge and the gap below it, a slider click repaints it,
      and Metal matches the interpreter within 2/255
      (`examples/10-custom-shader`).
- [x] `uniform time: F32` with continuous repaint while a time-reading shader is on
      screen, measured. An optional reserved uniform of the brush profile
      (`BrushLayout::time`, carried by the package codec), the seconds since the
      window's first frame from the frame instant (deterministic headless, kept
      across a reload); written into each draw's inline uniforms as its command is
      built (`Renderer::set_time`), so a clock-only frame uploads nothing. The
      lowering counts the clock-reading boxes it draws (`FrameStats::clock_instances`,
      `Renderer::reads_time`) and the window keeps beating like an animation while
      any is drawn; a program box missing its clip and the surface is culled before
      lowering, so one scrolled away, off the target or gone stops the beat. Release
      bench (1k quads + 64 clock boxes): `upload` 56.3 µs still, 55.6 µs moving,
      zero bytes and no buffer growth per clock frame; a live window drawing the
      example's ripples runs ~8% CPU where an idle one is at 0%. Tests:
      `render/tests/program_draw.rs`, `shader::program::brush` tests, the example's
      `the_rings_spread_with_the_clock` and Metal at a moved clock.
- [x] Texture and sampler parameters: a brush samples images its nodes name, through
      samplers it declares.
  - [x] Shader: the brush profile admits `texture name: Texture2D<Vec4F32>` as a
        texture parameter (at most `PROGRAM_TEXTURES`) and `sampler` members (at
        most `PROGRAM_SAMPLERS`), each sampler's state static in the layout
        (`BrushLayout::{textures, samplers}`, a `SamplerDesc` each, linear and
        clamp by default); `abi()` counts them; the package codec carries them, a
        decoded CPU layout checked against its program but for the samplers' state.
  - [x] DSL: `@sampler(filter: nearest | linear, address: clamp | repeat | mirror)`
        on a `sampler` member, checked (`E8105`: unknown setting or value, set
        twice, two `@sampler`s, on another member); a texture parameter is a field
        after the instance parameters taking an `ImageAsset`, an `SvgAsset` or
        other value `E2103` (`dsl/tests/shader_brush.rs`). Trilinear waits for
        asset mip chains.
  - [x] UI and view: `Content::Shader { images }` names each texture
        parameter's resource and the texture resident for it (`BrushImage`); the
        store resolves it through the asset table as an asset node does
        (`NodeStore::set_shader(.., images)`, the asset's nodes include those
        sampling it), a texture attached or replaced repaints only them (`PAINT`),
        and paint emits no draw until every image is resident; delivery splits the
        value's fields into the encoded parameters and the images' resources
        (`encode_shader`) and offers each image's bytes as an asset node does; the
        macro embeds the image, the release package and the dev patch carry it
        (`dsl/tests/view_assets.rs`).
  - [x] Render: `ShaderDraw::textures` (`ProgramTextures`); a program draw binds
        one bind group of its textures then the program's samplers (samplers
        interned per descriptor through the sampler cache, bind groups cached per
        program and texture set, released with the texture or the program); the
        segment's resource is the bind group, so draws of one program merge only
        over the same textures; a texture change is a resource change of the
        retained draw, no instance re-upload (`render/tests/program_draw.rs`).
  - [x] Example and tests: a `Lens` brush in `10-custom-shader` samples
        `assets/images/checker.png` through a nearest, repeating sampler with a
        running wave — headless shows only the image's two colors once it is
        resident, Metal matches the interpreter within 2/255; a texture swap
        repaints one node and uploads no instance; nearest against linear sampling
        differ as declared.

Reload (makepad keeps three caches — by script object, by function hash, by
generated code — never evicts, and compiles on the frame; take the code-keyed
reuse, drop the leak and the frame compile):

- [x] Source-keyed pipeline cache: an edit that leaves generated code unchanged
      reuses the existing pipeline, and a bounded cache evicts pipelines no slot
      draws with.
  - [x] A package carries no source span (`shader/src/program/package.rs`): a
        shader an edit only moved packs to the same bytes; `load_shader`'s weak
        cache is process-wide (a patch loads on the dev link's reader, the build's
        shaders on the loop) and compares the bytes, not only their hash; the dev
        host packs what the build embeds (the runtime's language and the reference
        interpreter's), so a reload loads the program already running: same slot,
        nothing installed. `ShaderSlots::replace` / `NodeStore::replace_shader`
        removed — a changed shader is another program its nodes move to.
  - [x] Renderer: pipelines kept by generated code and ABI (`ProgramTable`
        pipelines, a CPU program by identity), counted per slot drawing with them;
        a program whose code is held compiles nothing (`Renderer::program_compiles`);
        a pipeline no slot draws with stays idle, at most `IDLE_PIPELINES` (8), the
        longest unused retired first through the fence; a caller's own pipeline
        (`set_program`) goes with its last slot; `adopt_program` takes a program
        compiled off the render thread, `holds_program` says whether one is held;
        `FrameStats::program_pipelines`. Tests: `render/tests/program_draw.rs`
        (two slots of one code, 1000 edits bounded, an undo compiles nothing),
        `program::tests`, `dsl/tests/view_shaders.rs` (a moved shader keeps its
        program and slot), `viso::shaders::tests` (painted again, no compile).
- [x] Off-frame candidate compile and atomic commit (`viso/src/hot_reload/compile.rs`):
      staging a patch hands each program its views paint with that a window's
      renderer does not hold (`Renderer::holds_program`), with that window's
      `program_compiler` and surface format (`BrushProgram::desc`), to one compile
      thread; the patch commits at the first frame boundary after its compiles
      finished — patches behind it wait in order — and the commit hands each
      program to its renderer (`adopt_program`) before the views commit, so the
      frame's sync installs without compiling and the old pipelines retire
      through the fence (§21). A program the backend refuses NACKs the whole
      revision at `shader-compile` (`NACK_UNCOMPILABLE_SHADER`) after an error
      line naming the file, the shader and the driver's message — view, games and
      assets with it, the old pipeline still drawing (§19, §40). The ACK goes out
      at the commit, which nothing refuses after the compiles. A window opened
      meanwhile compiles on its first sync, as at startup; without a compile
      thread the frame compiles, as it does a window's first.
- [x] Host side: the host parses, checks and validates a shader's IR (`E8…` at
      `shader-compile`, nothing sent), generates the code the build embeds — the
      runtime's backend language and the reference interpreter's program — and
      sends it inside the view patch, one revision for the file's view, games and
      shaders; the backend validation is the runtime's compile, per target (§43).
      Shaders are declared in the `.vs` file that paints with them — a view
      compiles its file alone — so no external shader file is watched and no
      separate shader patch exists.
- [x] Interface change: a brush's parameters and images are encoded anew from the
      view's values by the new layout at commit (`encode_shader`), the record's
      shape checked against the shader at compile time (`E2103`); a program the
      runtime's renderer cannot draw — no code in its backend's language, a layout
      a draw cannot carry, more textures or samplers than a bind group holds
      (`ProgramError::Bindings`) — is a `gpu-validate` NACK at staging
      (`NACK_UNBINDABLE_SHADER`), before anything compiles or commits (§20).
- [x] `ProgramReload` (the reduced in-app design: compile from source on the
      runtime, migrate instance bytes by member name) removed with its Metal test —
      the runtime compiles no shader source.
- [x] Tests (§62 Shader), in the hot reload session's tests: an edit leaving the
      shader as it was compiles nothing and keeps its slot and pipeline; a code edit
      compiles on the compile thread, the frame installs it without compiling, the
      old slot empties; an interface edit encodes the node's instance by the new
      layout; a shader the renderer cannot draw is NACKed at staging with the view
      kept; on Metal, a shader the backend refuses is NACKed with the view and the
      pipeline kept and still drawing the old tint, and the next valid edit draws
      the new one. A retired pipeline outliving its fence is the retire queue's
      (`gpu/src/retire.rs` tests); pipelines bounded over 1000 edits is
      `render/tests/program_draw.rs`.

### Done

- [x] Editing a shader in a running app swaps it at a frame boundary; a syntax error
      never blanks the screen: a front-end error never leaves the host, a backend
      refusal NACKs the revision with the last good drawing.

---

## H6 — Phase D: Rust warm restart and DevSnapshot (§24–§32, §68)

Goal: a Rust edit rebuilds while the old app keeps running, then the app restarts with
its compatible state restored.

Nothing of it exists: `viso run` builds once, launches once and ends with the app;
the watch scope leaves Rust out; the host's `Expect` fixes the build id for the
session; only game snapshots encode to bytes; windows have no position API. Makepad
carries no state across a relaunch; its remote tool kills a spawned process's whole
tree (a job object on Windows, the process group elsewhere) — take that for a
cancelled build, whose `rustc`s must not outlive their `cargo`.

### H6.1 — Build coordinator

- [x] A bounded JSON reader in `viso-ende` (`JsonValue::parse` into a value tree,
      nesting bounded, RFC 8259 strict) for the tool's reading of Cargo's output —
      `cargo metadata` and the build's artifact messages — beside the writer.
- [x] Rust scope from `cargo metadata --format-version 1 --filter-platform <host>`:
      the directory of every path package in the app package's dependency closure
      (the resolve graph from the root), and what else changes the artifact — the
      workspace's `Cargo.lock`, `.cargo/config.toml`, `rust-toolchain{,.toml}`; read
      once at session start, and again when a `Cargo.toml` in it changes.
- [x] Watcher: several roots (a path dependency outside the project), each with its
      own `target/` excluded; the Rust files of the scope (`.rs`, `Cargo.toml`,
      `build.rs`, the files above) are tracked by content hash without being
      delivered at the first scan, so the build's own state is the baseline and a
      touch or a save that changed nothing builds nothing.
- [x] `RustBuild` (`tools/cli/src/dev/build.rs`): the launch build and every
      rebuild run `cargo build` of the dev artifact on a thread, its rendered
      diagnostics as log lines and its artifact messages read; the old app is never
      stopped before a build succeeds (§25, B.5). A Rust change while idle starts a
      build after the debounce window; one during a build kills it — its whole
      process tree — and starts the next, so the newest sources build and the
      newest successful build wins. A build
      whose executable is fresh (nothing relinked) restarts nothing. A failed build
      is a `warm-restart` reload event with Cargo's diagnostics, the old app kept.
- [x] A `.vs`-only change never builds (B.3); a Rust change that only touches
      `ui!`/`component!` builds — a Rust edit is always a warm restart.
- [x] Ctrl-C is `viso run`'s own (the build's process group no longer hears the
      terminal): the first kills the build and asks the app to close, a second
      kills the app; exit 130 with a `cancelled` summary.

### H6.2 — DevSnapshot (§29–§32)

- [x] `DevSnapshot { source_build, app_schema, revision, views, games, windows,
      extensions }` in ende binary (`crates/view/src/dev/snapshot.rs`, bounded on
      decode): per mounted view (`MountRef`: file path, window ordinal, mount
      ordinal) its state cells by `StateKey`, its behavior's states by slot as plain
      data (a closure or handle is not captured), and the focus, scroll offset, text
      and selection, and active child of each node holding one, by `NodeRef` (with
      its `for` item keys for a region node); per game its `GameSnapshot` bytes; per
      extension its bytes (the windows' geometry travels beside it, in the
      `Snapshot` message, for the host to launch with). Never pointers, handles, fds,
      sockets, task stacks or platform objects (§30). The states carry no schema: the
      host's plan from the old view knows their types.
- [x] `commit::restore`: the commit with the snapshot as the last-good — the view
      rebuilt from the new build's package, each kept cell seeded, each state through
      the plan (kept, converted, `@migrate`, reset with `E5101`), each kept node's
      state carried by the plan's node map; what no node takes reported lost. A
      carried edit buffer keeps its text over the rebuilt field's seed (also for a
      patch's carry).
- [x] Protocol 13: `HostMessage::{Snapshot, Restore}`, `RuntimeMessage::{Blob,
      Snapshot, Restored}` — the snapshot travels as blob chunks both ways (the
      runtime's writer thread cuts them, so the snapshot never fills or is dropped
      from the outgoing queue); the restore carries each view's package and plan
      from the view the old app ran to the one the new build embeds, the host's as
      for a patch. Blob totals bound at 256 MiB, assets still at 64 MiB.
- [x] Capture at the frame boundary after the request, after the patches commit:
      every window's logical size and outer position, every view mount, every game session (`Scheduler::snapshot`),
      every extension the app registered (`AppCx::dev_state(name, capture,
      restore)`, kept with viso-ui's `hot-reload` feature, dropped without).
- [x] Restore before the first frame of a process launched to restore
      (`VISO_DEV_RESTORE`; it waits 5 s for the restore after its first mounts):
      each mount of a window opened before the first frame boundary through its
      file's plan; games through `Scheduler::restore`; extensions by name; a restore
      not for the launch, whose snapshot did not arrive whole or does not decode, or
      whose view does not load is a `snapshot-restore` NACK and the app runs fresh;
      `Restored` at the first frame boundary reports what carried and reset per
      file, per game, and what no mount or registration took.
- [x] Host: the snapshot assembled from its chunks and kept in memory for the
      session only, never printed in logs or JSON (only its size and capture time)
      (§32, §47); the restore's plans from the last-good of each mount the old app
      ran to the view the new build reports in its first mounts.
- [x] `DevSnapshot` and its messages join the release-absence markers
      (`xtask/src/absence.rs`).
- [ ] An opt-in disk cache under the ignored dev cache (§32, "if ever allowed") waits
      for a DSL way to mark a state sensitive; it would exclude those and secure text
      fields.

### H6.3 — Desktop warm restart (§26)

- [x] Platform: a window's outer position and size read back, and a window created
      at a given position (`WindowConfig::position`) on macOS, Windows and X11;
      Wayland places windows itself, so only the size carries there.
- [x] Sequence: build success → request snapshot (3 s) → ask the old process to
      close (SIGTERM; killed after 3 s) → launch the new one with the restore flag →
      handshake (the build id is the session's; revisions restart at `LAUNCH`) →
      restore → resume; the mounts re-report and the session plans the next edit
      against what the new build embeds. A newer build during the restart replaces
      it; the old connection's late messages are dropped by `RuntimeSessionId`.
- [x] Launch the new process with the old windows' geometry: the host passes the
      first window's as `VISO_DEV_WINDOW=w,h[,x,y]`, the app opens its main window
      with it; a position on no screen falls back to the platform's placement.
- [x] `WARM_RESTART_REQUIRED` class for a Rust change: one `dev` event per restart,
      `outcome: warm_restarted`, once the restore lands (`rejected` when the new
      executable does not launch).
- [x] A restart ends whatever the new app does: restored (`restored: true`), its
      restore refused, its handshake refused (another Viso compiler built it — the
      only way the schema a `.vs` edit compiles against can differ from the app's,
      since natives come with the Viso version, so no `.vs` edit needs a restart),
      its channel closed or the app exited before restoring (`restored: false`); the
      session ending before the old app closed is `rejected`.
- [ ] An app that mounts no `view!` opens the dev channel too (at launch, not at
      the first mount), so its windows and extensions carry across a restart.
- [ ] Integration test (§62 Rust): app with state, edit Rust, assert old app runs
      until the build succeeds, warm restart, assert state restored; a failing build
      leaves the old app running.
- [x] Measurements: Rust incremental build time, snapshot time, relaunch time,
      restore time (§48), in the `warm-restart` event: `build_ms`, `snapshot_ms`,
      `close_ms`, `launch_ms`, `restore_ms`, `restart_ms`; the session polls the
      old app every 1 ms while it closes (6 ms, was 64 ms at the 50 ms poll).
- [ ] The new process's own startup is the restart (about 0.7 s of 0.8 s for the
      debug `examples/persist`): profile the dev artifact's launch to its first
      window.

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
