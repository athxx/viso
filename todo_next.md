# Viso next program — N1 → N10

What the architecture audit of `Viso_Architecture.md` against the code left to build. Every
part of the document was checked claim by claim:

- Where the code's design proved better, the document now describes the code.
- Where the document's design is better, or a target is still the right one, the work is
  listed here.
- Targets that were superseded were dropped from the document and are not listed: `#[component]`,
  a wgpu backend, an app-side `cx.query`, a `Painter` trait, a global `Store`, `integrations/`.

Rules (as for `todo_features.md`):
- Each item lands with its tests and with docs (spec text or ADR) in the same commit as the code.
- An item is ticked in the commit that completes it, with a short note of what was built and how
  it was verified.
- Order inside a section is by value.

## N1 — Release, footprint and boundary gates

- [x] Keep the behavior interpreter out of desktop, mobile and web release artifacts (release and
  shipping builds drop `viso/vm`, as embedded ones already do). Add an interpreter-only marker to
  `check-release-absence`. Verify that the scan fails when `viso/vm` is forced on and that the
  `binary` row of `cargo xtask footprint` shrinks.
  — Built: `vm` left the facade's defaults and the `viso new` template; the CLI adds `viso/vm` to
  every target's dev build. The interpreter loop's text is a release-absence marker. Verified:
  `check-release-absence` OK (15 markers in dev, none in release), and the vm-on release build
  carried the marker. `viso-example-i18n` release went from 8 208 816 to 8 175 328 B. The footprint
  bench mounts no `view!`, so its row does not move.
- [x] Make `check-profiles` resolve the release feature set (no `vm`) and reject
  `viso-behavior/interpreter` through `cargo tree -e features`. Verify that a profile with `viso/vm`
  added reports the path.
  — Built: embedded profiles without `vm`, a desktop profile on the defaults, and a feature check
  that names the chain turning the feature on. `ash` is optional on every target again; since the
  Vulkan rebuild it had been linked into every embedded closure. Verified: OK on aarch64 and
  armv7. With `viso/vm` forced on it reports `viso-behavior/interpreter <- viso-view feature "vm"
  <- viso feature "vm"`.
- [x] Run `check-profiles`, `check-targets` and `footprint --check` (against
  `benches/footprint/baseline.json`, with a tolerance) in CI. Verify that a seeded regression fails
  the job: `accesskit` added to the `kms` closure, or +10% heap or binary size.
  — Built: CI jobs `profiles-and-targets` (macOS, every app target) and `cost`.
  `cost` runs `footprint --against <base>`: the base commit is built from a detached worktree with
  its own target directory and measured on the same runner, with tighter paired tolerances (sizes
  1% + 4 KiB, heap 5% + 64 KiB). A baseline taken on another machine reads only as a trend.
  Verified: paired against HEAD, the headless subjects were within tolerance; 320 KiB leaked at
  startup (+10% heap) failed with 3.36 MiB against 3.05 MiB. The accesskit path is covered by the
  `check-profiles` unit test. The workflow itself has not run on GitHub yet.
- [x] Add a benchmark regression gate beyond `check-frame-tax`: medians of `frame_loop`,
  `list_scroll`, `paint_scale` and `reactive_binding` against a stored baseline. Verify that a 20%
  slowdown injected into layout fails it.
  — Built: `cargo xtask check-benches --against <rev>`, in CI's `cost` job. A stored baseline does
  not carry across machines, so `rev` is built in its own worktree and run beside the work tree.
  Each bench target runs back to back on both sides, the first side alternating by round, and the
  judgment is the median of per-round ratios (tolerance 15% + 20 ns). `grid_layout` and
  `large_list` joined the set: the four named benches never relayout. Verified: identical code
  stayed within ±8.2% over 4 rounds; a spin added to `layout_on` failed `grid_relayout_*` by +37%
  to +62%.
- [x] A first-party module-level forbidden-import scanner for platform, ui, render, gpu and dsl,
  run by `check-deps`. Verify that a planted `use viso_widgets` in `viso-ui` fails it.
  — Built: `xtask/src/imports.rs` parses the sources with `syn`, test items
  aside, and resolves each path against its module. Rules: `NEVER` crates; OS APIs in ui
  arena/reactive/layout and text shaping; Ende on the ui/render hot path; DSL stage order; GPU
  backend-neutral modules. Resolution reads HIR types and the HIR walk lowers to IR in the same
  pass, so `resolve` and `hir` are held only from the IR's consumers. Verified: `check-deps` OK. A
  planted `use viso_widgets::Button` in `viso-ui` and a `std::time::Instant` in `layout` were both
  reported.
- [x] Tighten the allowlist to the manifests: the facade's `viso-dsl` and `viso-dev-protocol` move
  to `dev`; drop the facade's `viso-handle` and `viso-schema` and the services' `viso-runtime`.
  Verify that `check-deps` stays OK and that a normal `viso-dsl` dependency in the facade fails it.
  — Built: facade `dsl`/`dev-protocol` became dev edges, facade `handle`/`schema`
  and services' `runtime` were dropped, and a stale facade manifest comment was fixed. Verified:
  `check-deps` OK, and a planted normal `viso-dsl` edge in the facade was refused.
- [x] Extend the duplicate-geometry names in `check-deps` (`Size`, `Insets`, `Aabb`, `Ray`,
  `Plane`, `Transform3`), and rename `viso_ui::layout::Size` (a length request) and
  `viso_map::camera::Insets` (f64). Verify that `check-deps` passes with the longer list.
  — Built: `Size`, `Insets`, `Transform3`, `Aabb`, `Ray` and `Plane` are
  math-owned. `viso_ui::Size` became `Sizing` across about 300 files, by a token-aware rename that
  leaves the `Size` variants of `Notice`, `CursorError` and the others alone. `viso_map::Insets`
  became `Padding`. Verified: `check-deps` OK, the workspace and `examples/map` check with all
  targets, and the `viso-ui` lib tests pass (769).
- [x] Clippy `undocumented_unsafe_blocks` on every member (unsafe stays allowed everywhere; the lint
  only asks each block for its `SAFETY:` invariant). Verify that clippy passes on every target once
  the blocks missing `SAFETY:` are documented, mostly in `crates/platform/src/backend/macos.rs`.
  — Built: `[workspace.lints.clippy]` in the root manifest, `[lints] workspace = true` in every
  member, and `SAFETY:` on every block (the counting allocators of the tests and benches, the
  Apple, iOS, Windows, ALSA, Vulkan and media backends). Fixed on the way: `ash` is no longer
  built for the web, rav1d (nightly-only on 32-bit ARM) is left out of armv7 like the web, and
  three test-only cfg slips. Verified: `check-targets` clean on all 8 targets, `check-profiles` OK.
- [x] Decide `libs/base64` and `libs/lz4`: make each a member with a user, move it to `vendor/`,
  or delete it. Verify that the workspace member list and `check-deps` agree.
  — Built: base64 is `viso_ende::base64` (either alphabet in, padding optional), replacing the
  copies in `viso-svg` data URLs, glTF embedded buffers and the CLI's web deploy; `viso-package`
  keeps its own by contract. Both non-member libs had no user and are deleted. Verified: the
  ende, svg, glTF and CLI web tests pass, `check-deps` OK.

## N2 — Public API

- [x] Control builders return their typed handle: `build(cx) -> Handle<T>` (`Copy`, id only),
  replacing the `*HandleSlot` (`Rc<RefCell<Option<H>>>`) idiom, about 50 uses. Verify that every
  widget test that used a slot compiles against the returned handle, and that the
  `ModalHandle`-style methods still act through `EventCx`.
  - Built: `viso_ui::control` (`ControlId`, `ControlTable` in the `StateStore`, `Controls`,
    `control_handle!`). Each control has its own `Copy` handle (a generic `Handle<T>` cannot
    carry widget-crate methods under the orphan rule). `build(cx)` returns it, and
    `cx.handle()` + `.handle(h)` cover forward references. The root node owns the parts and
    the table holds them weakly. `free_node` now also drops handlers. ADR 0054 (Proposed).
    All `*HandleSlot` aliases are gone from widgets, charts, map, the facade tests, benches
    and `examples/map`.
  - Verified: viso-ui lib (772), viso-widgets lib/tests/doctests, viso-charts and viso-map
    tests, and the facade widget tests (headless and default) pass. Clippy `--all-targets` is
    clean on all of them and on `examples/map`.
- [x] A typed Rust state handle `State<T>` over `StateId` for the `StateValue` kinds
  (`cx.state_of(0i32)`, typed `get`/`set`). Verify that `examples/01-counter` has no `StateValue`
  match.
  - Built: `viso_ui::typed_state`: `State<T>` (`Copy`, `get`/`set`/`update`), `StateKind` for
    i32/f32/bool/Rgba, `StateRead` (StateWriter's new supertrait; the effect and compute contexts
    record the read), `BuildCx::state_of`, and `bind` taking `impl Into<StateId>`. `State` is
    in the prelude. The counter, links and map examples use it. Architecture §18 updated.
  - Verified: typed_state unit and doc tests pass. Clippy is clean on viso-ui, viso and the
    examples. `examples/01-counter` has no `StateValue`.
- [x] `Application::on_lifecycle(&mut self, cx: &mut EventCx<'_>, Lifecycle)`, with `Lifecycle`
  re-exported. It is called on the launch window's context in this order:
  - on Suspended, before the persist flush;
  - on Resumed, after the redraw request;
  - on LowMemory, after the trim.

  Verify with a headless test that scripts the three events, checks the order, and checks that a
  `@persist` state written in the Suspended hook is durable.
  - Built: the hook with a default body, `viso::Lifecycle`, and `at_root`, which it shares with
    `on_open_url` and which runs on the launch window's root context. Architecture §50 and §78
    updated.
  - Verified: `examples/persist/tests/lifecycle.rs` (the facade crate grants no
    `storage.persist`) scripts Suspended/Resumed/LowMemory. It checks the order, that the store
    already held the hook's write when the app resumed, and that a later launch loads it.
    Moving the hook after the flush makes the test fail.
- [x] On Suspended, pause the playing videos and the game's audio output; on Resumed, resume only
  those the framework paused. Verify with the scripted player and a fake audio output.
  - Built: `Videos::suspend`/`resume`, where a resume plays a held video only if the app still
    asks for it. `FrameClient::suspend`/`resume`, reached from `NodeStore::suspend` and the new
    `NodeStore::resume`. `GameSession` holds the device it paused (`GameAudioDevice::pause`/
    `resume`). `AudioOutput::pause`/`resume`/`is_running` keep the render in a shared slot that
    only the running device touches, so every backend restarts on the same render.
    Architecture §50 updated.
  - Verified: the videos unit test covers a playing video, one the app paused, and one the app
    paused while suspended. `game_session` audio uses a fake device that pauses once, resumes
    once, and is not started when it was not playing. check-targets is clean on android, wasm32,
    windows, linux and macOS.
- [x] Make game sessions' `@persist` states durable on Suspended, through a store suspend hook.
  Verify with a `game_persist` test that suspends without dropping the session.
  - Built: `GameSession::suspend`, reached through `FrameClient::suspend` from
    `NodeStore::suspend`, runs `Scheduler::suspend` and queues the scheduler's persist reports
    for `take_persist_reports`. Architecture §50 updated.
  - Verified: `game_persist` mounts a packaged game granted `storage.persist`, runs it five
    ticks, and suspends the store. Nothing is durable before; the session still runs; a second
    launch loads the value from the suspend. Without `Scheduler::suspend` the test fails.
- [x] Typed routes: a Rust `Route` enum, `Navigator<R>`, `RouterView` and `routes!` (DSL U11),
  serving `NavigationStack` and `on_open_url`. Verify with a DSL test and a UI scenario that push,
  pop, system back and a deep link keep the back-stack page's state.
  - Rust side built: `viso_ui::router` provides `Navigator<R>` (a `Copy` control handle; reads
    through `StateRead + Controls`, commands through any `StateWriter`), `RouterView` (pages
    keyed by route value, one `Template` per variant, popped pages freed once they have left,
    focus moved to the new page's heading, `on_back_requested` with `prevent_default`), and
    `routes!`. `NodeStore::turn_page_from` turns between page nodes. The prelude exports them.
    Verified by the router unit tests and `crates/viso/tests/router.rs` (deep links, system
    back, and the inbox's state kept). Still to do: the DSL side.
  - DSL native enums built: `.vs` reads a schema enum as an enum its library declares (the
    symbol `resolve::native_enum(path)`), so it builds, compares and exhaustively matches them;
    `#[derive(NativeData)]` on a Rust enum gives variants with tuple or named fields
    (`NativeType::payloads`, carried in `natives.json`). Verified by `crates/viso/tests/native_data.rs`
    and the game test suites.
  - Generic native types and `viso::nav` built: `NativeType::params` with `SchemaTy::Param`,
    `Ty::Native(id, args)`; `Navigator<R>` (in scope without an import) with `current`,
    `can_pop`, `depth`, `push`, `replace`, `pop`, `reset` over a host `RouteStack`. ADR 0055
    (Proposed). Verified by `crates/dsl/tests/navigator.rs`.
  - DSL side built: `view!("shell.vs", nav: router, ...)` passes root inputs through
    `viso_view::ViewInput` (scalars by value; a `RouterView<R>` becomes the `Navigator<R>`, its
    slot tracked through the navigator's revision cell; a value of another type, an unknown name
    or a missing required input does not compile). The `RouterView` node (`ControlKind::Router`)
    builds the Rust router view inside itself once and fires `back_requested(BackRequest)` before
    a system back pops; `e.prevent_default()` keeps the page. Native package discovery takes the
    app's own `[package.metadata.viso.natives]` (`crate::NATIVES` in its library). `routes!`
    arms may return anything, so a page may be a `view!`. Docs: DSL U11, Architecture §6.1.
  - Verified: `examples/04-navigation/tests/navigation.rs` — the `.vs` shell pushes and pops
    the Rust router, its title and back button follow the route, a system back is kept while
    editing (fails without `prevent_default`), a deep link resets the stack over the inbox, and
    the inbox's draft survives every turn. Discovery of the app's own natives by a
    `native_package` unit test. An `InputMap<E>` constant is found again now that native types
    carry type arguments (`game_acceptance`, `game_input`); the cross-platform snapshot hashes
    are repinned to the rebuilt modules.
- [x] Typed Rust access to named nodes from `view!`/`component!` (`nodes.save_button:
  Handle<Button>`). Verify with a trybuild test that a wrong-type use fails to compile.
  - Built: `viso_ui::Handle<K = AnyNode>` with a `viso_ui::kind` marker per standard widget
    (checked against the schema's widget table); `view!` returns `MountedView { root, nodes }`
    and `component!` builds `(Self, MountedView<{Name}Nodes>)`, a `Handle<kind::K>` field per
    named node of the root component that it builds once (none for nodes under `if`/`for`/
    `match` or of inlined components). `view!` inputs now type-check by kind
    (`ViewInput::Kind`), so a wrong-type value fails `cargo check`; an input with a default it
    is not given holds the default. Docs: Architecture §10.4.11 and §78, DSL §22.1, AGENTS §6.3.
  - Verified: `crates/viso/tests/named_nodes.rs` (the handles are the built nodes, by role and
    label), `view_inputs.rs` (given and defaulted inputs), and the trybuild cases
    `named_node_kind` (`Handle<kind::Text>` from a `Button` fails), `view_input_type` and
    `view_input_missing`; the facade's 29 macro-using test binaries pass.
- [x] `ui!` mounts any `viso::ui::Component`, so hand-written widgets and `component!` structs have
  one shape. Verify with a `ui!` case naming a hand-written `impl Component`.
  - Built: `viso_ui::Mount` (`fn mount(cx) -> Handle`), implemented for every `Component +
    Default` and by each `component!` struct; `ui!` places a Rust component node through it.
    `BuildCx::component(&c)` builds any `Component` and returns its root. Docs: Architecture
    §6, DSL §40.1.
  - Verified: `crates/viso/tests/ui_macro.rs` mounts a hand-written `Swatch` and a `component!`
    `Tally` side by side, and a `Swatch` as a fragment's root.
- [x] Add `Component`, `Sizing`, `Align`, `Justify`, `Axis`, `Inset`, `BoxStyle` and `Rgba` to the
  prelude, with `component!`/`view!` re-exported from it. Verify that the examples build with
  `use viso::prelude::*` as their only `viso` import.
  - Built: those, `EventCx` (every handler and `on_open_url` takes it) and `component!`/`view!`
    beside `ui!`. `01-counter` clicks a `button` widget built with `cx.component`, so it needs
    no pointer phases. Architecture §5.1 and Appendix E.
  - Verified: `hello_world`, `01-counter`, `links`, `shell`'s and `04-navigation`'s views
    import only the prelude from `viso` for UI (`shell` adds its tray module, `04-navigation`
    `viso::native`); `pointer` and `code-editor` keep only pointer, cursor, text-style and key
    types. `cargo check --workspace --all-targets` passes.
- [x] Split `crates/viso/src/lib.rs` (5.6k lines: `AppDriver`, per-window state, frame phases,
  access keys) into modules, leaving docs, `mod` and `pub use`. Verify that the `cargo doc` item list
  is unchanged and that the facade tests pass.
  — Built: `lib.rs` (196 lines) keeps the crate docs, `mod`, `pub use` and the re-export modules.
  `app` holds `Application` and `run`; `driver/` holds `AppDriver` (`mod`), per-window state
  (`window_state`), the frame phases (`frame`), access keys and the platform conversions;
  `prelude`, `stdio`, `behavior` and `test_support` hold the prelude, the print macros, the
  behavior macros and the test harness. Verified: `all.html` lists the same items (rustdoc adds only
  redirects under `app/`), `cargo doc` has no warnings, clippy `-D warnings` is clean on default and
  `headless,hot-reload`, and the build is checked on default, `headless`, `hot-reload` and both. The
  lib tests pass except a stale byte count (fixed separately). The facade integration tests pass
  except `game_sheet` (golden drift, fails at HEAD too) and `game_audio_device` (no device output
  here).
- [x] Remove dead surface:
  - the marker contexts `LayoutCx`/`PaintCx`/`RenderCx`/`TaskCx`;
  - `viso_ui::token` (`TokenId`/`TokenInterner`/its `Theme`) with `StyleId`/`resolve_styles`/
    `set_style_token`;
  - `TranslateAnim`/`AnimationRegistry`/`EventCx::request_animation` (port
    `tests/animation_loop.rs` to `animate`);
  - `WireId`/`IdKind`;
  - the `mobile` feature.

  Verify with `cargo check --workspace --all-targets`.
  — Built: all of it is gone, together with the router's and the store's animation hand-off
  queues and the driver's registry, the `style_resolve` bench, the `style_token` test and the
  inspector's `style` row. A handler now animates through `EventCx::animate(node, to, timing)`,
  a store request applied after it returns like the others. The `animation_tick` bench times
  `tick_transitions` over translate animations. `animation_loop` checks that the end notice
  reports finished at the target and that the loop halts, and, three frames in, that world moved
  with nothing relaid. Verified: `cargo check --workspace --all-targets` and clippy `-D warnings`
  (workspace, and `viso`/`viso-ui` with `headless,hot-reload`) are clean; the viso-ui lib (761),
  `animation_loop` (3) and viso-ende tests pass; viso-ext and the examples use none of it.
- [x] Produce or remove `RedrawReason::TimerDue`/`HotReload` and
  `FrameDecision::BackgroundMaintenance`. Verify with scheduler unit tests.
  — Built: removed. Reasons feed only `decide()`, and a timer deadline and a dev-channel message
  both arrive as `RawEvent::Wakeup` (`AsyncCompletion`), so separate variants would change
  nothing. `RedrawReasons` is a `u8` again. Verified: viso-runtime lib tests (37) pass; workspace
  clippy `-D warnings` is clean.
- [x] `viso::version()`. Verify that it matches `CARGO_PKG_VERSION`.
  — Built: a `const fn` at the crate root returning the facade's package version. Verified: a unit
  test compares it with `CARGO_PKG_VERSION`; clippy `-D warnings` is clean.

## N3 — Robustness and security

- [x] Bounded collection reads in `viso-ende`: `Decoder::read_count(max)` checks the cap and
  `count <= remaining()`. Replace the three hand copies in `schema/src/codec.rs`,
  `behavior/src/wire.rs` and `dev-protocol/src/wire.rs`. Verify that a huge count over zero-byte
  items fails fast.
  — Built: `read_count(max)` (over the cap: the new `DecodeError::TooMany`; over the bytes left:
  `UnexpectedEof`) and `read_list(max, item)`, which reserves at most 4096 items. The schema and
  behavior `read_list`s, dev-protocol's `read_count`/`read_list` (mapping `TooMany` to
  `WireError::Bound`), the AOT track reader and the 13 region-table counts use them. Verified:
  an ende test reads a `u64::MAX` count with zero-byte items and fails at once, reading none;
  ende, schema, dev-protocol and behavior lib tests pass; the workspace checks clean.
- [x] cargo-fuzz targets for `GameNet::receive` packets, AOT and view packages, behavior modules,
  the raster, SVG, glTF and font decoders, and text segmentation. Verify that each runs 10 minutes
  in `fuzz.yml` without a crash, with `encode(decode(x))` stable.
  — Built: nine targets (`game_packet`, `aot_package`, `view_package`, `behavior_module`, `raster`,
  `svg`, `gltf`, `font`, `text`); the package and module targets check that the encoding of what
  decodes decodes to the same encoding. `crates/dsl/tests/fuzz_seeds.rs` writes their seeds, and
  `fuzz.yml` runs every target 600 s after running all three seed writers. `raster` found two
  allocations sized by a frame header: an APNG `fcTL` past the canvas (zune-png sizes its output by
  the frame) and a GIF frame past the pixel cap. Each is refused before allocating, with the crash
  inputs kept as fixtures. Verified locally: each target ran 60 s without a crash and `raster` ran
  5 min after the fixes. The 10-minute CI runs happen on the nightly schedule, not here.
- [x] Record `FINGERPRINT_VERSION` in persisted artifacts (view package, persist store) and refuse
  a mismatch with a diagnostic. Verify with a test that bumps the version and gets the error
  instead of silently dropped state. — Built: the constant moved to `viso_schema` (the DSL
  re-exports it). The view package (now `VPK2`) and each persisted value (`VPS2`) write it after
  their magic. A different version is `DecodeError::Version { offset, found, expected }`.
  `load_view` says to rebuild the package, and `Persistence::load` reports the value as not
  loaded. A `VPS1` value from before this change is read as version 1. Verified: tests that bump
  the version in a package (schema, and through `load_view` in view) and in a persisted value
  (behavior) get the error, and a `VPS1` value still loads. ende/schema/dev-protocol/dsl resolve
  tests pass, workspace check is clean, and clippy -D warnings is clean on the changed crates.
- [x] Recreate the GPU device after a loss (D3D12 removed or reset, `VK_ERROR_DEVICE_LOST`,
  WebGPU `device.lost`) and re-upload from the asset tables. Verify by forcing a removal and
  checking that the next frame redraws without a panic.
  — Built:
  - The RHI's old `device_lost` is renamed `abandon_frame`, and the new `GpuBackend::is_lost`
    reports a device that is gone. Each backend latches the loss from its API: D3D12
    removed/reset at present; Vulkan `ERROR_DEVICE_LOST` from submit, acquire, present or a
    fence wait; Metal command buffers that fail with `DeviceRemoved`; WebGPU `device.lost`;
    GLES `EGL_CONTEXT_LOST`. Once lost, no frame begins on it and nothing is encoded.
  - `try_create_device` opens a new device. In a browser it asks WebGPU for one
    asynchronously.
  - On loss the window drops its GPU state and its text stack, and forgets every GPU object
    in its tables: asset textures and models, scene slots, brush programs, video textures.
    It then rebuilds on the new device: it shapes its text again, re-decodes its assets from
    the encoded bytes the table now keeps (or from their origin), and paints the whole tree.
    While no device opens, it retries with a 0.25–4 s backoff.
  - Replacing the renderer after a surface-format change now also reinstalls brush programs
    and refills scenes.
  - ADR 0056.

  Verified:
  - `tests/device_loss.rs` loses the headless device (`CpuRaster::lose_device`) after each
    frame of a window showing text and a photo. Every run redraws on a new device, byte for
    byte identical, with no panic. The test fails if either the asset forget or the text
    re-shape is removed.
  - A unit test in the ui asset table covers forget and re-decode.
  - Clippy -D warnings is clean on host, windows-msvc, wasm32, and linux vulkan and gles.
  - `check-deps` passes.
  - Not run: a real removal on Windows (`D3D12Backend::remove_device` exists for that). Losses
    first seen while creating a resource can still panic in the native backends' create paths.
- [x] Fixed-capacity `NodeStore` under the embedded profiles, sized from the footprint profile; a
  full store reports `ArenaFull` instead of panicking. Verify with an embedded-cpu headless test
  that overfills it.
  — Built:
  - `MemoryBudget::nodes` sets the window store's capacity: embedded-gpu 16384, embedded-cpu
    4096, the others grow. An app can change it in `Application::memory_budget`.
  - A window builds its store with `NodeStore::with_fixed_capacity`, with one column slot past
    the capacity. A node created when the store is full is refused: it gets a handle that is
    never live, writes into the spare slot, and every write or link through it does nothing.
  - `take_refused` reports `ArenaFull` with the refused count, and the facade prints it once a
    frame.
  - Not done: a `Viso.toml` budget key for `nodes`, because the budget keys there are byte sizes.

  Verified:
  - A ui unit test overfills a store of capacity 8: 7 refused, what fit lays out and paints,
    and a freed node makes room again.
  - `tests/node_budget.rs` builds 40 rows of label, button and text input at a 48-node budget.
    It passes headless both on the desktop profile and with `VISO_FOOTPRINT=embedded-cpu`: no
    panic, the store full, the root laid out, and the `ArenaFull` line printed.
  - Clippy -D warnings is clean.
- [x] `ProtocolTag` bad magic returns `DecodeError::Malformed { offset }`. Ende golden-byte tests
  run on a 32-bit target. Verify both in CI.
  — Built:
  - A bad magic in `ProtocolTag` is now `Malformed` at the offset where the tag starts.
  - `crates/ende/tests/golden.rs` fixes the exact bytes of every scalar, varint, string and
    byte string, a two-section table with its aligned offsets, and `content_hash`. It also
    checks that a length or count past `u32` is an error rather than a wrapped value, and that
    a foreign preamble is malformed at its offset.
  - CI has a new `ende-32bit` job that runs `cargo test -p viso-ende` on
    `i686-unknown-linux-gnu`.
  - The huge-count unit test now accepts `TooMany` as well as EOF: on a 32-bit target the count
    is past `usize`.

  Verified:
  - Every ende test passes on the host. They also pass on i686: built for
    `i686-unknown-linux-musl` and run in an emulated `linux/386` container. That run found the
    unit test that assumed a 64-bit `usize`.
  - The schema, dev-protocol and behavior tests pass.
  - The CI job itself has not run yet.
- [x] A debug-only owner-store tag on `NodeId`, with an assertion on cross-window use. Verify with
  a debug test that uses window A's handle on window B.
  - Built: each `NodeArena` takes an owner number from a global counter in a debug build and stamps
    every handle it issues. `is_live` and `NodeArena::slot` assert that a handle is the arena's own.
    Every `NodeStore` accessor that indexes a column by a handle goes through `slot`. Equality,
    hashing and `Debug` ignore the stamp. A release build has no field, so `NodeId` stays 8 bytes.
    A refused handle belongs to no arena.
  - Verified: two debug tests build two windows' trees, each with a node at slot 0, and use A's
    handle on B. Reading B's bounds panics, and so does asking B whether the handle is live. A third
    test checks own-window use and a refused handle on either store.
  - Three ui tests used another store's handle to stand for a stale one. They now use a refused
    handle.
  - The ui, dsl and multi-window facade tests pass. Clippy is clean on debug and release builds.

## N4 — Performance

- [x] Pack the per-node bool columns (`hittable`, `hidden`, `clips`, `overlay`, `chrome`,
  `focusable`, `disabled`, `transform_root`) into one `NodeFlags` column. Verify with
  `node_arena`/`hover_diff` benches and a per-node bytes assertion.
  - Built: one `NodeFlags` byte holds the eight flags, and a fresh node is `FRESH` (hittable only).
    The column list macro now names each column once. It also sums one slot of every column, which
    `NodeStore::bytes_per_node` reports, the arena's `SLOT_BYTES` included.
  - Built: the `turn` column was missing from the list, so `clear` and `reserve` skipped it. It is
    now in the list.
  - Verified: a test pins `bytes_per_node` at 602 on 64-bit targets, down from 609, and checks that
    `NodeFlags` is one byte. The ui lib tests and clippy pass.
  - Verified: `node_arena` and `hover_diff` ran A/B at the default profile and at
    `opt-level=3`. Every row is within noise (±1.5%).
- [x] Move rarely set dense `Option` columns (`semantics`, `semantic_state`, `check_marks`,
  `interaction`) to sparse storage. Verify that column bytes drop and `semantic_projection` does not
  regress.
  - Built: a `Sparse<T>` column keeps a `u32` per slot and packs the values, each with its slot.
    Taking a value out moves the last one into its place. The four columns moved to it, and so did
    the two handler columns (`handlers`, `key_handlers`).
  - Built: handler presence reads only the slot word. Setting a semantic state looks its slot up
    once.
  - Verified: `bytes_per_node` drops from 602 to 314. A unit test covers insert, remove, set and
    in-place change.
  - Verified: three interleaved `semantic_projection` A/B runs at the default profile.
    `project_wake` is +0.3–2%, `derive_with_state` is +1–6%, and
    `derive_full_single_label_change` is +0.5–3%. The cost is the extra lookup per sparse read.
    That is within noise to a few percent, not a clean zero.
  - The ui lib, viso lib and clippy runs pass.
- [x] Derive semantics per dirty subtree: keep the last tree and diff only re-derived branches.
  Verify on 10k nodes that one label change re-derives O(depth) nodes and that the delta equals a
  full re-derive's.
  - Built: a node's row is `semantics_row` (with `semantics_link_row`), and its read children come
    from `read_children`. The whole-tree derive composes them.
  - Built: the accessibility bridge keeps its last published nodes. A SEMANTICS-only frame
    re-derives only the SEMANTICS-marked branches. A row whose `disabled` changed re-derives its
    subtree. A `semantics.hidden` flip removes or adds a published subtree. A frame with LAYOUT,
    TRANSFORM or STRUCTURE re-derives the whole tree and diffs it. The focus is published even on
    a frame with no dirty mark.
  - Built: setters that feed a row now mark SEMANTICS on change: focusable, handler presence,
    access key, and a refresh starting or ending. An error node's label marks the fields that cite
    it.
  - Verified: on 10,101 nodes, one label change derives 3 rows and publishes one node. A clean
    frame derives 0.
  - Verified: a differential test runs twelve kinds of change through the incremental bridge and a
    whole-tree bridge in lockstep. Deltas, published trees and targets are equal. Sabotage confirmed
    that the test catches a dropped `disabled` inheritance and a dropped error-relation mark.
  - The ui and viso lib tests, `access_keys`, `semantics_snapshot` and clippy pass.
- [x] Skip re-measuring clean subtrees inside a relayout root. Verify with the incremental-vs-full
  differential test and a 2000-row Column bench.
  - Built: one settled bit per node (`SlotBits`) is set when the node is measured. A MEASURE mark,
    a `LayoutInput` write or an absolute-rows extent clears it on the node and its ancestors, up to
    the first one already clear. This crosses layout boundaries, so a change inside a fixed box is
    reached from a redo root above it. `layout::remeasure` walks only unsettled children, and
    `relayout_dirty` uses it; the whole-tree `layout` still measures everything. A mark cleared
    unconsumed at frame end leaves the bit clear.
  - Built: `relayout_dirty` reports the nodes it actually measured.
  - Verified: the flex differential test now runs twin stores. One only relayouts incrementally,
    keeping its sizes across frames; the other lays out whole each frame. Bounds are compared on
    shown nodes every frame for 24 seeds × 200 frames. Fewer than half the nodes under the redo
    roots are re-measured.
  - Verified: a new twin-store stacks/absolutes test checks the same for stacks and absolutes.
  - Verified: the new `column_relayout` bench (2000 rows, one label change) runs 275 µs → 218 µs
    at the default profile. Two interleaved runs.
  - Found, not fixed: running the stacks/absolutes repaint check on one store across consecutive
    incremental frames, with no whole layout between them, gives one paint-order mismatch (seed 3,
    frame 114: a stack child shown again is spliced after a later row's child). This happens with
    measure skipping disabled too, so the bug is in `paint::splice_walk` and predates this change.
    Tracked below.
  - The ui lib and integration tests, viso lib, dsl `virtual_list`/`hot_reload`/`node_refs` tests
    and clippy pass.
- [x] Fix the incremental repaint splice that places a re-shown stack child after a later row's
  primitives. Verify with the twin-store stacks/absolutes test checking `repaint_dirty` against
  `paint_tree` every frame.
  - Cause: a node that paints nothing (every child hidden, no own drawables) has an empty record
    at one stream position. When a splice inserted primitives at exactly that position for a node
    after it in paint order, the record shift moved the empty record past the inserted run. A
    later child shown under that node was then spliced after the other node's primitives.
  - Built: when moving the records of unvisited nodes, an empty record at a position where an edit
    inserted primitives is forgotten. Whether it sat before or after them is not known. A later
    change there finds no record and paints whole.
  - Verified: the twin-store stacks/absolutes test (now
    `kept_sizes_relayout_and_repaint_as_whole_ones_over_stacks_and_absolutes`) checks
    `repaint_dirty` against `paint_tree` every frame and passes.
  - Verified: a focused splice test fails without the fix and passes with it. Two boxes paint
    nothing but a hidden child each; the second's child is shown, then the first's.
  - The ui lib tests, including the splice fuzz tests' spliced/patched/whole ratios, and clippy pass.
- [x] Drive glyph representation promotion at runtime: MTSDF for runs under sustained scale or
  rotation, settling back to A8, with real MTSDF pages. Verify with:
  - a scaled/rotated text golden;
  - counters that stay 0 for pure translation;
  - Text §29.1's MTSDF rows.
  - Built: each frame, after the transforms update, `TransformTable::turned_text` walks only the
    turned roots' subtrees and finds the shown text under them, with their linear part. Each such
    run drives its own `RepresentationState` from the mean scale and whether it is rotated.
    Fields are generated on the worker (`ToWorker::Fields`, `MtsdfGenerator`) into a real MTSDF
    plane (RGBA8 data, edge ≤ 1024, its own page budget). The plane is uploaded, reclaimed per
    page and kept by trim. A run switches at a frame boundary once its whole glyph set is
    resident, and draws last-good coverage until then. Settling rasterizes exact coverage at the
    held size, drawn at the laid-out extent. Releasing the turn returns the run to its own
    coverage and drops its state. Switching only repaints (`set_painted_content`).
    `Content::Text` gained `lane`, and paint emits it. Counters: representation evaluations,
    promotions, demotions and fields, reported in the frame stats and `TextStats`. The window
    wakes at the earliest representation deadline.
  - Also fixed: the MTSDF shader's distance range ignored the place and the display density,
    which softened edges under any turn or HiDPI. It now reads texels per pixel off the uv's
    screen derivatives. The MSL oracle and SPIR-V were re-blessed.
  - Not done: there is no OutlineVector lane, so past the top bucket the top field is magnified.
  - Verified: the golden `scaled_and_rotated_text_draws_from_fields_and_matches_its_golden`
    renders "bd" at 3× and 0.35 rad through CpuRaster on the MTSDF lane. Against GPU-stretched
    coverage it has 577 fully lit and 290 edge pixels, versus 64 and 1140.
  - Verified: `text_that_only_moves_takes_no_look_and_counts_nothing` translates for 40 frames.
    Evaluations, promotions, demotions and fields stay 0, and no plane is created.
  - Verified: zoom, held-rotation and static-scale tests cover sustained promotion (not before
    100 ms), field quads covering the coverage boxes, settle-back to 64 px coverage, and return
    to 16 px with the look dropped. A held rotation is not evaluated again.
  - Verified: `turned_text` picks only the shown text under turns, each once. Text §29.1 rows
    are updated. The viso, ui and text lib tests and the shader and mtsdf_lane tests pass.
    clippy is clean on ui, text, shader, render and viso (host, headless, wasm32, no
    rasterizer).
- [x] Reorder-safe spans in the batch planner, so text over sibling cards rejoins one glyph draw.
  Verify with planner order tests and a draw-count drop in `paint_scale`.
  - Built: `batch::plan` folds segments into draws. A segment may join an earlier draw when its
    instances follow that draw's in the family buffer and every draw it passes is in another
    pass, or in its space with a known cover that does not overlap its own. The cover is paint
    bounds or content extent, plus extrusion and 1 px of AA, cut by the clip. The walk back
    stops at the first draw it cannot pass, or after 32 draws. Program draws never move.
  - Built: segments keep their paint-order meaning, so patch and splice are unchanged. The
    encoder, `frame_stats` and inspect read the draws. Planning runs per 64-segment chunk, and
    an in-place splice replans only the chunks it touched.
  - Verified: 9 planner order tests (fold, overlap barrier, unknown extent, other space, other
    pass, contiguity, clip, program, reach), `batch_planner.rs` and `texture_binding.rs`.
  - Verified: renderer tests show the planned draws match drawing the segments in paint order
    pixel for pixel. A spliced label replans its chunk and matches a fresh frame.
  - Verified: in `paint_scale`, `type/notes_100` drops from 200 draws to 13 and
    `type/notes_2000` from 4000 to 131; `mount/rows_160` drops from 2 to 1. Frame time costs
    +0.5 µs (1.21→1.70 µs and 1.34→1.84 µs); mount is within 0.3 µs.
- [x] `Encoder::clear` and `Encoder::from_vec`; reuse one encoder for game net packets and dev
  frames. Verify that a counting allocator sees 0 allocations per steady-state packet.
  - Built: `Encoder::clear` keeps the capacity, and `Encoder::from_vec` takes a buffer back.
  - Built: `RollbackSession::write_packet_for` writes a packet into a caller's encoder straight
    from the input and hash queues, with no intermediate lists. `packet_for` wraps it.
  - Built: the dev protocol gains `frame_into` (a frame built in an encoder) and
    `write_frame_with`. The native dev link's writer thread and the web link's send path each
    reuse one encoder.
  - Verified: `dsl/tests/steady_packets.rs` runs a counting global allocator. Two peers exchange
    packets; after warm-up, 180 packets into one cleared encoder allocate 0 times and match
    `packet_for`'s bytes. Sabotage with one allocation in the writer gave 180.
  - Verified: `dev-protocol/tests/steady_frames.rs` sees 0 allocations over 200 frames.
  - The `game_netplay` and `fuzz_seeds` tests pass, and clippy is clean on the touched crates and
    on viso `hot-reload` native and wasm32.
- [x] Derive the inline work slice from the measured frame interval and share it across windows.
  Verify at 120 Hz emulation that the slice holds with N windows.
  - Built: the driver's `FramePace` smooths the frame deltas into an interval estimate, ignoring
    idle gaps over 100 ms. Each frame earns a quarter of its delta, capped at a quarter interval.
    Generic inline jobs, inline text shaping and inline asset decodes, across every window, spend
    from that one account and are charged what they took. A spent account runs nothing inline,
    and an overrun is a debt that later frames repay. The text commit budget now follows the
    measured interval. `TextShaper::pump` takes the inline slice and returns the shaping time.
    `Assets::serve` takes the slice and returns `Served { deferred, inline }`. The three fixed
    4 ms slices are gone. Headless stays unlimited.
  - Verified: `driver::pace` tests emulate 120 Hz with 1, 2, 4 and 8 windows, each beat running a
    frame with three consumers per window, and jobs of 0, 300 µs and 3 ms past each slice. Over
    2 s the inline time stays between 95% of a quarter of the clock and a quarter plus one head
    start and one job. The interval converges to 8.33 ms, and a frame after idle holds one
    interval's share. text_content, assets and playback lib tests pass. clippy is clean on viso
    host, headless and wasm32.
- [x] Cancel a freed node's tasks through a per-node chain instead of scanning every task. Verify
  with a bench that frees 1k rows with 100 live tasks.
  - Built: `TaskSet` keeps tasks in a slab, each owner's tasks on a doubly linked chain, with chain
    heads sorted by owner key (`TaskOwner::key`, a node's slot). Wakers carry `(id, slot)` and
    poll checks the id. `cancel_owned` walks only the owner's chain, and a slot's new node shares
    the chain but keeps its own tasks. `NodeStore` cancels a freed node's tasks through it.
  - Verified: the `ui/benches/task_cancel` A/B frees 1000 rows (3k nodes) one at a time with 100
    pending tasks. Tasks on other nodes: 124 → 45.5 µs. Tasks on the freed rows: 91 → 54 µs. What
    remains is the free itself.
  - Verified: runtime chain tests cover cancels and finishes from the middle, slot reuse, and a
    key sharer being spared. The ui test checks that freeing a subtree drops exactly its nodes'
    tasks, including on a reused slot. viso `task_loop`/`dsl_tasks` pass, and clippy is clean.
- [x] Rust fragment functions generated at build time from Shader IR for CpuRaster user programs.
  Verify that they are bit-exact against `CpuShader` and faster in the `cpu_raster` bench.
  - Built: the interpreter's ops and control flow moved into a shared `rt` module; `native::emit`
    writes a program's entries as Rust calling the same fns with constant registers, into a
    `NativeKernel` checked by program hash and register count. `view!` emits it for the CPU
    backend only (`__per_backend!`'s `none` arm); `__embedded_shader` hands it to
    `CpuShader::with_native`. A hidden `__shader_kernel!` builds one from a source literal.
  - Verified: shader_golden draws both scenes with the kernel and the interpreter and compares them
    byte for byte; a mutated kernel fails the test. dsl_shaders checks that `view!` runs the kernel
    under `headless` and not on GPU builds. cpu_raster 800×480 user shaders: rounded 12.4 → 7.3 ms,
    ripple (function + loop) 78.5 → 15.5 ms, with identical pixels. Clippy is clean (host,
    headless, wasm).
- [x] The missing benches:
  - `layout_10k_dirty_{1pct,100pct}`, `hit_test_100k`, `state_update_1k_bindings`;
  - `startup_*`, `hot_reload_*`, `ende_*`, `identity_*`;
  - `damage_single_label_update`, `memory_10k_nodes`;
  - widget microbenches after Tier 4 (DataGrid, Calendar, ColorPicker, DatePicker, NumberField).

  Verify that `cargo bench -- --list` shows each one.
  - Built: 85 benches. viso-ui: layout_scale, hit_scale, state_update, identity_scale,
    memory_nodes. viso-ende: codec. viso-dsl: identity, hot_reload, ende_payloads (the payloads
    other crates carry live here, so ende keeps no viso dev-deps). viso: startup, damage_label,
    damage_scroll (headless). viso-widgets: calendar, date_picker, number_field, color_picker and
    data_grid builds. Each asserts its work once and prints what it measures.
  - Verified: `--list` shows every named bench (`reactive_hot_reload_rebind` is left as a target:
    it is a viso-view patch commit). Clippy `--benches` is clean in all five crates, and
    check-deps passes. Indicative times: layout_10k_dirty 118 µs / 1.58 ms, hit_test_100k 731 ns,
    state_update_1k 19 µs, startup 228 µs / 1.32 ms, damage_single_label 22 µs/tick,
    hot_reload small 1.7 ms / large 112 ms.
- [x] Follow-ups the new benches found:
  - [x] `color_picker/paint_tree` costs 356 µs because the tone mesh is rebuilt with per-vertex
    HSV every paint; cache it or fill it with a shader. Done: the tone, ring, checkerboard and
    hue/alpha track meshes are built about the origin, kept in a 32-entry cache keyed by what they
    are drawn from, and placed with `MeshPlace`. paint_tree went from 54.6 µs to 0.76 µs, and the
    golden still matches. The picker tests had reused one run's node handles in another run's
    tree, which the debug owner check rejects; they now resolve each run's parts in it.
  - [x] `data_grid/layout` costs 138 µs for 200 cells; check it. Done: on a quiet machine it is
    26 µs, as fast per node as the other layouts, so the 138 µs was load from parallel builds.
    The profile showed `concealed` asking the folds and pages for every node, so a per-slot
    `choosers` bit now gates that on the parent having been folded or paged. That is neutral
    here and saves the scans in trees with a pager or a fold.
  - [x] Virtual-list rows with `Length::fill()` seem to take the surface's width rather than the
    list's (or the list doesn't clip), so a scroll damages the whole surface. Not a bug: the bench
    lays the list out as the root, and the root stretches to the surface.
  - [x] `resolve` calls `path.display()` per comparison, which allocates and is quadratic in the
    module count. Done: resolve and lowering index the units by module path once (one interner,
    so paths compare by segment). Neutral at 110 modules (link 52.6 → 52.5 ms), with no
    allocation per module pair.
  - [x] A small component recompile costs about 1.7 ms of fixed cost (natives, prelude); profile it.
    The prelude was parsed and resolved twice a compile (by resolve and by lowering), over half the
    time. Now each thread parses it once, and one compile resolves it once and lowers against that.
    hot_reload_small_component 1.17 → 0.50 ms.
  - [x] `JsonWriter` can't reuse its buffer, and `RollbackSession::receive` allocates two Vecs a
    packet. `JsonWriter::with_buffer` and `clear` keep a buffer, and the CLI's event stream reuses
    one line. A reused write of 1k diagnostics allocates nothing (was 20, 1 MB). Escaping now
    copies unescaped runs whole, which halves the write (681 → 343 µs). `receive` reads the
    inputs in place and the hashes into a fixed array: 0 allocations a packet (was 2, 104 B),
    86.6 → 60.8 ns a round trip.
  - [x] `DrivenApp` has no damage accessor. `DrivenApp::damage` gives the last submit's region.
    damage_single_label_update now asserts that the region covers the pixels the tick changed:
    one 475 px rect around a 77 px box.

  Verify each with its bench.
- [x] Hot, warm and cold bytes per node in `viso inspect frame`. Verify that the counter app is
  ≤ 64 hot bytes per node.
  - Built: `NodeStore::memory()` → `NodeMemory {nodes, hot, warm, cold}` over the per-node columns
    and opt-in side tables, tiered by what reads them; `inspect frame` reports a `nodes` resource
    group with bytes and B/node. Scroll, translate and turn (nonzero on few nodes) moved into a
    `Sparse<Motion>` side table that drops an entry once it rests; `rules_at` and `opacity` are
    warm (layout and paint read them).
  - Verified: a plain node is 63 hot bytes (was 94); the 01-counter tree reports hot 63 / warm 233 /
    cold 156 B/node and the inspect test asserts ≤ 64. A/B on a quiet machine: hover ±2%,
    animation_tick/1 +0.6 ns, list_100k_scroll −2–4%. viso-ui, widgets and viso lib tests and the
    scroll/sheet/virtual-list tests pass; clippy is clean.

- [x] Repair the viso integration tests that went stale: calendar, date_picker, menu, splitter,
  tabs, time_picker and tooltip panic on the owner check; game_sheet departs from its golden;
  keyboard_routing and pointer_routing find a node dirtied at write time.
  - Built: the tests read a node off one scripted run and look it up on the next, which held while
    handles were plain indices. Arena owners now count per thread, and the headless harness numbers
    each run's trees from the first again, so identical scripts issue identical handles. Windows of
    one run still differ. Taking a key or pointer handler re-derives a node's semantics, so the
    routing tests assert that the write adds nothing to the node's dirt rather than that it has
    none. The sheet's golden predates PBR shading; a bisect puts the change at the PBR materials
    commit, and the golden is re-blessed.
  - Verified: the ten suites pass unchanged in their scripts, and viso-ui's lib tests pass.

## N5 — Rendering, text, input and accessibility

- [x] On the web without WebGPU, draw through CpuRaster into the canvas (`putImageData`) instead of
  leaving it blank. Verify in headless Chromium with WebGPU disabled against the headless golden.
  - Built: wasm's `Backend` is `WebBackend`, `Gpu(WebGpuBackend)` or `Cpu(CpuRaster)`, chosen once
    the page asked for a device. `GpuBackend::shader_lang()` tells the language at run time; the
    renderer keeps it and takes built-in, scene and user program code by it, and hot reload
    compiles for it. A CPU-drawn page window gets a canvas 2D `Scanout`: each frame's damage, BGRA
    to RGBA, goes in by `putImageData`, the whole frame when the size changed. User brushes run on
    the interpreter from the `None` program every pack carries. CpuRaster keeps only the workers
    the platform starts, so a page draws on its own thread. The counter example holds its
    full-screen headless frame to `golden/counter.png`.
  - Verified: 01-counter in headless Chrome, once with no `navigator.gpu` and once with an adapter
    request answering null, draws on the 2D context and matches the headless golden pixel for
    pixel at 800×600, as the WebGPU run does. After three clicks and after a resize to 640×420,
    the CPU and WebGPU frames are identical. The custom-shader example's brushes paint on the CPU.
    Clippy is clean on host, headless and wasm (with and without hot reload).
- [x] Controls take their default colors from the env `theme` instead of fixed dark-palette
  constants. Verify with snapshots under the light and dark themes, and that a theme swap marks
  PAINT only.
  - Built:
    - The default styles of `viso-widgets` and the `viso-ui` controls name `ColorToken`s, and so
      do the DSL's unbound defaults, the edit menu's list and the window's background.
    - The store resolves tokens on every color write, including the inks of rich-text spans,
      and repaints them on `set_palette`. Text fields' selection, caret and handles resolve at
      paint time.
    - `fade` derives a tint from a token or a plain color. Part setters resolve before they
      compare, so writing the same value stays a no-op.
    - A scroll-bar style that only recolors no longer re-lays the bars out.
  - Verified (`crates/viso/tests/theme_controls.rs`):
    - A gallery of eleven controls is held to `theme_light.png` and `theme_dark.png`, and no
      token reaches the paint.
    - A system light→dark swap frame measures and lays out nothing, and draws the same pixels as
      starting in dark.
    - The per-widget goldens on dark scenes now draw in the dark palette and are re-blessed.
    - A rich text's span ink and fill follow a palette swap with PAINT only, and a reflowed run
      draws resolved too (`viso-ui` theme tests).
- [x] Assistive-technology scroll actions on scroll viewports and virtual lists (`ScrollDown`,
  `ScrollUp`, `ScrollIntoView`, iOS `accessibilityScroll`). Verify with an AccessKit request test
  that reaches an unmounted row of a 10k-row list.
  - Built:
    - Viewports publish `scroll_x`/`scroll_y` and offer `ScrollUp`/`Down`/`Left`/`Right` for each
      way they can still move. Nodes inside a viewport offer `ScrollIntoView`.
    - `NodeStore::scroll_page` turns the nearest viewport that can move a page at once (a page
      key's step, or onto an item when it snaps). `ScrollIntoView` reveals the node.
    - AccessKit maps both ways and states the orientation. iOS `accessibilityScroll:` walks the
      mirror up to a viewport that can scroll, then posts "Page n of m".
    - A virtual list is `Role::List`.
    - Fixed a virtual list that mounted only its overscan rows until first scrolled: reconcile
      ran before the first layout gave the viewport its extent. Reconcile now also gates on the
      viewport's extent, and a frame whose layout resized a list reconciles again.
  - Verified:
    - `crates/viso/tests/access_scroll.rs`, a 10,000-row list driven with an assistive technology
      listening: three `ScrollDown` requests publish rows never mounted, with their places of
      10,000, while the mounted window stays a window. `ScrollUp` turns back, and a row cut off at
      the end comes into view on `ScrollIntoView`. The first frame mounts all ten rows the
      viewport covers.
    - The AccessKit bridge maps scroll requests and state; the mirror finds the scrolling
      ancestor; the iOS page-status and direction tests pass.
    - Clippy is clean on host, iOS, Android, Windows and wasm.
- [x] Surrounding text and selection to platform IMEs:
  - macOS `attributedSubstringForProposedRange`/`selectedRange`;
  - iOS `UITextInput`;
  - Android `getTextBeforeCursor`;
  - Windows TSF/IMM reconversion;
  - Wayland `set_surrounding_text`.

  Verify with macOS Japanese reconversion and Android autocorrect.
  - Built:
    - A focused field that shows its text publishes `SurroundingText` at the frame's end: about
      1 KiB either side of the selection, the composition included, with its byte offset in the
      whole text. It is rebuilt only when the revision, selection or composition changes, and
      its start holds still while the caret moves on nearby, so input-method offsets keep their
      meaning between edits. Secrets, read-only fields and selections over 16 KiB publish none.
    - Input methods edit by range through `RawEvent::ImeEdit`: `Replace`, `Recompose` (committed
      text becomes the composition again) and `Select`. The buffer keeps a composition the edit
      stays clear of, moving it with the text, and keeps a selection the edit stays clear of.
    - `ImeText`, a platform-side mirror of the published text, answers queries at once and
      applies each edit the way the app will until the app publishes again. macOS, iOS, Windows
      and Wayland share it; Android's input connection mirrors it in its `Editable`.
    - macOS: `selectedRange`, `markedRange`, `attributedSubstringForProposedRange` and the
      replacement ranges (reconversion, press-and-hold accents). iOS: `UITextInput` over the
      text, with autocorrection and spell checking on while the text is shown. Android:
      `getTextBeforeCursor`, `setComposingRegion`, `deleteSurroundingText`, `setSelection` and
      the initial surrounding text, with auto-correct asked for. Windows: `IMR_RECONVERTSTRING`,
      `IMR_CONFIRMRECONVERTSTRING` and `IMR_DOCUMENTFEED`. Wayland: `set_surrounding_text`
      (within the protocol's 4000 bytes) and `delete_surrounding_text`.
    - UI tapes record and replay the ranged edits.
  - Verified:
    - `crates/viso/tests/ime_surrounding.rs`: a focused field shows its text; an autocorrect
      replaces the typed word and keeps the caret; a reconversion composes over committed text
      and commits; a deletion before a composition keeps it composing; the input method moves
      the selection; a secret shows nothing.
    - Mirror tests replay a Japanese reconversion (`setMarkedText` over the selection and over
      the word before the caret), an autocorrect, deletions around a composition, and text
      shown before the app took a preedit. The Windows `RECONVERTSTRING` layout and the Wayland
      `done` order and 4000-byte cut pass, the Wayland ones in a Linux container.
    - The Android Java compiles against API 36. Clippy is clean on host, iOS, Android, Windows,
      Linux and wasm.
    - Not yet seen on a device: this Mac has no Japanese input source, and the API 37 emulator
      image offers no Vulkan, so macOS Japanese reconversion and Gboard autocorrect still need a
      hands-on check.
- [x] IME clause segments and the selected clause, drawn with a thicker underline on the active
  clause. Verify with headless preedit replay and a Japanese IME.
  - Built:
    - `PreeditClauses` (byte ranges within the preedit, and the one converting) rides every
      preedit from the platform to `EditIntent::Compose` and into `ImeComposition`.
    - macOS reads `NSMarkedClauseSegment` and thick-underline runs, falling back to a ranged
      `selectedRange`; iOS uses a ranged `selectedRange`.
    - Android reads the composing text's `UnderlineSpan` and `BackgroundColorSpan`.
    - Windows reads `GCS_COMPCLAUSE` and `GCS_COMPATTR`; Wayland uses a ranged preedit cursor.
    - X11 tracks XIM's per-character feedback through each draw and splits it into runs.
    - Each clause gets its own thin underline with a gap between, and the converting one is
      twice as thick. A selection with no clauses splits the composition in three.
    - UI tapes record the clauses under a new tag; plain preedits keep the old one.
  - Verified:
    - `crates/viso/tests/ime_surrounding.rs` (headless): two clauses draw two underlines with a
      gap, the converting one thicker; a plain preedit draws one. A recorded composition
      round-trips through the tape's bytes and replays the same underlines.
    - Unit tests: the marks for clauses and for a bare selection, mark runs over UTF-16, and
      the tape's encoding and rejection of malformed clauses. X11 feedback splicing and the
      Wayland ranged cursor pass in a Linux container; the Windows clause and attribute test
      compiles but is not run here.
    - The Android Java compiles. Clippy is clean on host, iOS, Android, Windows, Linux and wasm.
    - Not yet seen with a real Japanese IME: this Mac has no Japanese input source and the
      emulator has no Vulkan.
- [x] Merge the labels of a node marked as one element (such as a list row) into one semantics
  node. Verify in the headless semantics tree.
  - Built:
    - `Semantics::merged` (DSL `semantics.merge`, a constant flag). With no label of its own, a
      merged node is named by the labels of its subtree in tree order, joined by commas.
    - `read_children` skips the absorbed nodes. It yields in their place the nodes in the
      subtree that act on their own (focusable, handling input, holding links, or merged
      themselves), and those keep their labels.
    - The view module carries the flag in the live setting's high bit, so older modules still
      decode.
  - Verified:
    - A ui unit test: an icon, title and subtitle read as "Starred, Inbox, 3 new"; a hidden one
      is left out; a focusable button stays the only child; an authored label wins.
    - The bridge's differential test publishes the same tree incrementally as a whole re-derive
      when a group is merged, a label it absorbs changes, a node in it becomes focusable, and
      it is unmerged.
    - DSL tests: `semantics.merge: true` reaches the node, and a non-constant value is refused.
- [x] A wrapping Flow layout (`LayoutInput::Flow`, DSL `Wrap`). Verify with layout unit tests
  (gap, RTL, hidden children) and the differential layout.
  - Built:
    - `LayoutInput::Flow` with an axis, `gap`, `line_gap`, padding, `align` and `justify`, plus
      `BuildCx::flow(FlowStyle)`.
    - Children fill a line until the next would overflow; a line always holds at least one.
      A fill takes a whole line, and a fit is cut to the line. Lines are as thick as their
      thickest child and are stacked across.
    - Hidden children take no room and no gap. Right to left, lines start at the right.
    - The store keeps each flow's last main-axis extent in a small table sorted by slot, so
      there is no per-node column. Measure breaks lines at that extent using the same sizing
      as layout. When a layout lands at a new extent, the flow is marked MEASURE and layout
      runs again in the same frame, at most three times, the same way scroll bars do.
    - DSL `Wrap` takes `gap`, `row_gap` (the gap between lines, as in CSS), `justify` and
      `align`. These fold or bind like a flex's, through the macro path, the AOT package (a
      new node kind and a `line_gap` style bit) and the reload restyle.
    - The web export is `flex-wrap: wrap` with `column-gap` and `row-gap`; a fill child gets
      `width: 100%`.
  - Verified:
    - ui unit tests:
      - Lines with gaps, and the next sibling following them.
      - Narrower (one child per line) and wider (one line).
      - Right to left.
      - A hidden child.
      - Centred lines and children.
      - An overlong child and a fill.
    - The incremental-versus-whole differential layout test now includes flows with nine
      labels each, under every align and justify; it passes over 24 seeds × 200 frames.
    - DSL tests: `Wrap` lowers to a flow with its gaps and placements. A `ui!` wrap breaks
      lines, and a `row_gap` in `sp` follows the text scale. The HTML export wraps. The AOT
      round trip carries `line_gap`.
- [x] Typed `ShapedGlyph` cluster and glyph ids, and per-locale kinsoku for zh-Hans, zh-Hant, ja
  and ko. Verify with Text §29.1's open rows.
  - Built:
    - `RunOffset` (a byte offset within one shaped run) and `GlyphId`. They are carried from
      shaping through `GlyphKey`, the outline cache, MTSDF, the color rasterizer and the platform
      raster seams. Ids go back to `u16` only where font files are read.
    - `LineBreakLocale` resolves a BCP-47 tag into one of five rule sets: zh-Hans, zh-Hant, ja, ko
      or other. For `zh`, Traditional comes from the `Hant` script or a TW, HK or MO region.
    - Each locale adds its own prohibition table over ICU's `ja_zh` table:
      - Chinese and Japanese: curly quotation marks open and close. This adds breaks ICU
        withholds before an opening mark and after a closing one.
      - The name interpunct `·` never starts a line in Chinese. In Simplified it also never
        ends one.
      - Korean: quotation marks stay unresolved, so a particle stays attached to its closing mark.
    - Scoping:
      - Prohibitions apply at Strict and Normal only.
      - The quotation-mark breaks apply at every level except Anywhere, and not under keep-all.
    - The new breaks are found by a byte scan between the provider's breaks, so text with no
      curly quotation marks costs no extra decoding.
  - Verified:
    - Each tag resolves to its own locale.
    - One sentence breaks four different ways across the four locales.
    - Chinese and Japanese quotation marks open and close, and an apostrophe inside a word gets
      no break.
    - A Korean closing quotation mark keeps its particle.
    - Strictness and `keep-all` scope the rules.
    - The paragraph content locale carries each locale.
    - Cross-target clippy is clean on wasm32, Windows, Android and Linux.
- [x] Name `ReactiveCycle` (E4202) cells and effect nodes by state name and source span, routed to
  dev diagnostics. Verify with an E4202 sample in `viso dev`.
  - Built:
    - Each effect carries an `EffectOrigin` from whoever mounted it. A view's effect carries its
      body's handler-table entry.
    - A cycle across frames now also carries the cells that woke the stopped effects, not only
      their nodes.
    - `BuildCx::state` records its call site, so a Rust cell can be named by where it was made.
    - Under `viso run`, the runtime sends the cycle as a structured `RuntimeMessage::Diagnostic`
      (protocol 22):
      - A `.vs` state is labelled by file and `StateKey`.
      - An effect is labelled by the byte range of its body.
      - A Rust cell is labelled by its call site.
    - The CLI resolves each `StateKey` to its declaration in the version the app is running. It
      renders through the same source path as compile diagnostics. Labels it cannot place become
      notes.
    - Without a dev session the cycle still goes to stderr.
  - Verified:
    - A feedback effect in a `.vs` fixture raises E4202 across frames, naming `count` and the
      effect's `count += 1` body.
    - The CLI places a runtime diagnostic in the running source.
    - The wire round trip carries every label kind.
    - The flush and reactive suites pass with origins and cells.
- [x] One diagnostic shape shared by `viso-shader` and `viso-dsl`, in a leaf crate. Verify that the
  LSP and the CLI render shader diagnostics through one path.
  - Built:
    - The leaf `viso-diag` holds the source spans (`TextSize`, `TextRange`, `LineIndex`) and the one
      `Diagnostic` with its related spans, fixes and severity. `viso-lexer` and `viso_dsl::diag`
      re-export them at their old paths.
    - `viso-shader` drops its parallel `Diagnostic` and `ProgramError`:
      - Program validation, the instance check and the brush profile return spanned diagnostics.
        The DSL forwards them as they are, without a conversion.
      - A built-in body error converts with its span relative to its fragment.
      - The dev pipeline's ABI check points at no source, so it returns the typed `LayoutError`.
    - The LSP publishes a full check of the document, the same one `viso check` runs. Before, it
      published only parse and resolve diagnostics, so type and shader errors never reached the
      editor.
    - The LSP also carries a diagnostic's related spans in the document as `relatedInformation`,
      with its notes after the message.
  - Verified:
    - A brush-profile error from the shader compiler renders in `viso check` with its primary
      span, the view that paints with it and the `viso explain` pointer.
    - The same error publishes over the LSP at the shader's name, with the painting view as its
      related information.
    - The shader, LSP, DSL diag and brush, and CLI check suites pass.
    - `check-deps` passes.

## N6 — DSL and tooling

- [x] Native widgets and traits in native package schemas, so an app's or an extension's Rust
  widget is usable from `.vs`. Verify with a fixture package widget that passes `viso check` and
  mounts in a `view!`.
  - Built:
    - `natives.json` carries a library's traits and its Rust widgets under two optional keys, so
      existing schemas read unchanged. A package carries only Rust widgets: no slots, groups or
      bubbling events.
    - `NativeWidget::rust` declares a widget's own properties and events. Every node's layout,
      transform, focus, `semantics.*`, `transition.*`, look and standard events come from an
      implicit box.
    - A Rust widget node lowers as a Stack with `ControlKind::Rust`. Its own properties become
      entries, and its own events become handler indices in `RustEntries`, outside the event
      routes.
    - The write-back of a two-way property reads the whole payload, as the widget's change event
      carries the value itself.
    - At run time a `RustWidget` builds inside the node on its first delivery and takes each own
      property's value as it changes. It keeps its instance across a hot reload and fires its own
      events through `WidgetEvents`.
    - A package names its `&'static [WidgetImpl]` with the manifest key `widgets`. `view!` links
      those of the widgets the view names, and a release load takes them as `ViewLoad::widgets`.
      `check_widgets` keeps them in step with the schema.
    - `examples/11-native-widget` is an app whose panel binds the gauge of its `gauge` package.
  - Verified:
    - The fixture passes `viso check` from the package's schema; a CLI test covers this.
    - In a `view!` the gauge shows the bound level. A press on it writes the level back, it shows
      the new level, and the view's handler of its own event runs.
    - The DSL suite for Rust widgets passes: entries, events, the box's look and routes, type
      errors, unknown properties and children. The schema round-trip, native package and package
      schema tests also pass.
    - Clippy passes on the touched crates, including wasm32 for viso-view, and `check-deps` passes.
- [x] Move the CLI's `build/`, `dev/`, `device/`, `toolchain/` and inspect client into a shared
  tooling crate. Verify with `check-deps` that `viso-cli` is a thin consumer.
  - Built:
    - `viso-tooling` (`tools/tooling`) holds build, the dev session and its control endpoint,
      devices, toolchains, delivery and deploy. It also holds the program runner, atomic
      writes, interrupts, secrets and the error code table.
    - The services report through an object-safe `Reporter`: diagnostics in a `Source`,
      failures under a `Code`, logs, device states, and dev, rebuild, runtime and profile
      events. The CLI's `Output` implements it.
    - The inspector client is `viso_tooling::inspect`. It covers census, nodes, frame and
      events, typed `AskError`/`NoSession`, a project's session found by its lock, node
      labels, and a window's journal followed or streamed. The `viso inspect` command
      renders what it returns.
    - A child is interrupted through `process::interrupt`, so the CLI needs no `libc` or
      `windows` crate.
    - ADR 0057 records the boundary.
  - Verified:
    - `check-deps` passes with `viso-cli` limited to the tooling crate, the project model,
      the packager, the compiler, the dev protocol, the schema, Ende and the image comparer.
      Its `NEVER` edges forbid `viso-svg` and `viso-text`. `viso-tooling` is on the compiler
      side.
    - The tooling suite passes: 157 tests, the dev session's against a counting `Reporter`.
    - The CLI's unit tests and its check, inspect, json and run suites pass.
    - Clippy passes on host, Windows and Linux.
- [x] `web-dom` and `web-hybrid` runtime backends and the per-target capability table (the C4.3
  and C4.4 lines of `todo_cli.md`; the platform-gated natives' rows wait for a native missing on
  some target, carried over in N9). Verify that `viso build web-dom` no longer reports `C0011` and
  `viso check web-dom` flags a GPU-only node.
  - [x] `web-dom` build and check: `viso-dom` runs the view's behavior on the VM in the app's
    wasm; the page is the html export's lowering with its calls going to the VM.
    - Verified: `viso build web-dom` builds a `viso new` app and its page counts clicks in
      headless Chrome; `viso check web-dom` reports `WEB_GPU_ONLY_NODE` for a `ShaderView`
      (`export_dom` test); release wasm 912 KiB.
  - [x] `web-dom` dev session (`viso run`/`serve web-dom`): `.vs` edits rewrite the page and
    reload it without a Rust build. Verified on a `viso new` app in headless Chrome.
  - [x] Package natives in the page lowering: values calling them are computed by the page's VM.
  - [x] `web-hybrid` (ADR 0059): a DOM page with one component the renderer draws.
    - [x] `Island` in `.vs`: one component of the file, recorded in `ViewBehavior::islands` (its
      input and event entries); a stack of its child where nothing splits the view. Verified:
      `view_islands` tests (record, mount errors, package mount).
    - [x] Hybrid page lowering (`export::hybrid`): the island as a `data-viso-root` element, its
      content and its instances' effects left out, each bound input pushed with `V.i`, island
      events run through `V.listen`; a second island or one in a region is a finding. Verified:
      `export_dom` hybrid tests.
    - [x] Runtime: the facade's `web-hybrid` feature mounts the island's component as the root
      view; inputs from the page, events back to it. The app and the page script share one page
      (`viso_dom::shared`); `viso_dom::island` queues pushed values, which a task the root owns
      lands at a frame boundary (`ViewHost::push_input`), and emitted events
      (`ViewHost::set_event_sink`), which a microtask runs on the page. `view!` no longer
      rejects the states of a component it inlines. Verified: `dsl_island` test (the root, a
      pushed value landing, events running the page's handler), wasm check of both features.
    - [x] Target, bundle, `check`/`build`/`run`/`serve web-hybrid`: the target is available and
      builds with `viso/web-hybrid`; the page carries no `.vsb` (the app draws the island), and
      `visoDom` resolves once the app's `build` offered the view, which a hybrid app reaches
      after its WebGPU device. `WEB_HYBRID_NO_ISLAND` for a view without one. A `.vs` edit in
      `run web-hybrid` rebuilds the app, whose bundle writes the page lowered again; the
      watcher's first delivery does not. Verified: headless Chrome with WebGPU: page clicks
      push into the island, the island's click runs the page's handler, a `.vs` edit in
      `viso run` rebuilt and served the new island; `web-dom` page still runs; CLI
      check/doctor/json/golden and tooling/project unit tests.
- [x] `#[derive(Encode, Decode)]` in `viso-macros` for plain messages. Verify by porting one
  dev-protocol message and asserting identical bytes.
  - Built: the derive (struct fields in order; enum byte tags in declaration order,
    `#[ende(tag = N)]`; field `varint`/`max`/`with`; `crate = path`); `viso-ende` gains the
    shapes a message is built of (strings, lists with `Vec<u8>` as one run, options, tuples,
    arrays, `u128`) and their varint mirror (`EncodeVarint`/`DecodeVarint`).
  - Verified: `InspectAsk` (an enum) and `NodeSource` (a struct) ported; `derived_codecs` tests
    match their hand-written codecs byte for byte and read back; ende unit tests.
- [x] Extend the VM-vs-native differential test to every DSL doc-example fixture. Verify that it
  reports no mismatch.
  - Built: every ```viso block of the specification and the rationale that compiles to behavior
    (as written, or as a component's members, an action's body or a view's) is lowered into the
    checked-in `differential/docs.rs` (`VISO_BLESS=1`); each chunk a component owns runs on both
    sides with each value of a pool of every shape, twice over, a required input taking several
    shapes in turn, tasks driven to their end. The doc-block reader is shared with `doc_examples`.
  - Verified: 13 examples, 1176 runs, both forms (bytecode and compiled module), no mismatch.
- [x] A `stable / preview / reserved` matrix per grammar production in `Viso_DSL_1.0.md`. Verify
  with a doc test that every production is listed.
  - Built: Appendix A.14 gives each of the 213 productions a maturity: Core and Standard
    surface `stable`, the Advanced surface of §25.1 (traits/impls, general and const generics,
    `dyn`, template/part, handwritten native declarations, capability clauses) `preview`,
    `ComputeEntry` (no 1.0 profile has a compute stage, `E8106`) `reserved`; §25.1 and the
    Architecture's surface rules point at it.
  - Verified: `ebnf_golden` requires the table and the appendix's productions to match one to one
    with a known maturity each, and every reserved production's positive source to parse but not
    compile.

## N7 — Widgets and extras

- [x] `adaptive_split` (list-detail that collapses to a stack) and `adaptive_navigation` (bottom
  bar, rail or sidebar by size class). Verify with headless tests that resize across size classes
  and check the tree and the focus.
  - Built: both anchor the environment at their root and re-arrange the same nodes in a structure
    hook when the size class there changes. `adaptive_split` stands list and detail side by side
    from `side_by_side` (default expanded) up, else relinks them into a paged stack, the detail
    pushed over the list by `SplitHandle::show_detail` and popped by a back gesture; focus in a
    folded pane moves to the one in front, back where it last was there. `adaptive_navigation`
    lays its destinations out as a bottom bar, a rail or a sidebar (`layouts`), a roving tablist
    whose arrows follow its axis, over a paged content area.
  - Verified: `viso-widgets/tests/adaptive_layouts.rs` resizes compact, medium and expanded and
    checks parents, visibility, bounds, arrangement, selection and focus;
    `viso/tests/adaptive_widgets.rs` resizes a window under the frame loop.
- [x] Move PDF out of the core into `viso-ext`. The core keeps a public paged capture API: a
  view laid out at a page's width, cut into pages with `page_break`/`keep_together` and its
  header and footer, giving each page's primitives with glyph outlines and a raster callback for
  what has no vector form. `viso-pdf` (the writer) and `pdf::export` move to `viso-ext/viso-pdf`
  over that API, beside `viso-print`, and the facade's `pdf` feature goes. Verify that the
  moved export writes the same bytes for the existing PDF tests, and that `check-deps` and the
  facade build without it.
  - Built: `viso::paged::capture(Paged, take)` runs `take` in the frame on a `Capture`: each
    page's `PagePart`s (primitives and `TextRun`s in paint order) and a `PageSource` for glyph
    outlines (or platform masks) and rasters through the window's renderer. The writer, `Pdf`
    and `export` live in `viso-ext/viso-pdf` over it; `crates/pdf`, the `pdf` feature and its
    allowlist entries are gone.
  - Verified: the moved export test writes byte-identical output to the pre-move build (`cmp`),
    the writer's unit tests pass, `check-deps` is clean, and the facade checks and lints with
    defaults and with `--no-default-features --features rasterizer`.
- [x] A PDF viewer widget over `viso-pdf`'s reader side. Verify that a multi-page document renders
  and selects text in a golden test.
  - Built: `viso_pdf::read` opens PDF files (xref tables and streams, object streams, recovery by
    scan, RC4/AES, the common filters) and reads pages into paths, glyph outlines (TrueType, CFF,
    Type 1, Type 3, stand-in faces), images and a text layer, clips kept as boxes. `pdf_view` is a
    virtual column of pages built near the view as `Drawing`s in page points (the new
    `Drawing::scaling_strokes`, so zoom is a size change) with images as assets; it zooms (fit
    width, scale, pinch, Control-wheel, keys) and selects across pages by drag, word and line.
    The core gains `VirtualList::set_extents`; an oversized masked solid fill masks at a page;
    headless decodes assets inline; a window owes a frame while assets or a lost GPU are
    unserved; and a recovered device's first paint emits the whole tree.
  - Verified: `viso-pdf/tests/view.rs` renders a four-page report against goldens and checks the
    exact text of a drag, a cross-page drag, a double click and select all; the reader reads the
    export's documents back; a sweep of real files (to 645 pages, CJK, LaTeX Type 1, forms)
    reads without a panic at ~1 ms a page; `device_loss` and the asset, list and scroll tests pass.
- [x] A browser widget in `viso-ext/viso-webview`, which is still the cargo template. Verify that a
  page loads inside a node on macOS and Android.
  - Built: core native views (ADR 0060, `viso_ui::native`): `BuildCx::native_view` gives a node a
    platform view that hears its window box, clip, scale and visibility after layout when they
    change and goes with the node; `BuildCx::native_events` hands reports from any thread to the
    node's handler at the frame boundary. `viso-webview`'s `web_view` puts WKWebView (macOS) or
    `android.webkit.WebView` (Android, a Java class in an embedded dex) in a clipping host over the
    box, reports `WebEvent`s, and its `WebViewHandle` loads, moves through history and evaluates
    scripts. iOS and the other targets leave the box empty and report `Failed`.
  - Verified: `viso/tests/native_view.rs` checks placement under scroll and clip, hiding, the drop
    with the node and cross-thread reports. The example's check mode loads its page, runs a
    script and gets back the title, the text and the box's viewport on macOS (800x519) and on an
    Android 37 emulator (411x818, GLES there since the emulator's Vulkan would not start with the
    display asleep); a screenshot shows the page under the toolbar.
- [x] KMS: libinput gestures (pinch, swipe) as `RawEvent::Gesture`, and overlay planes for video.
  Verify with a libinput fixture, and a video scanned out on a board without the GLES pass.
  - Built: libinput's touchpad pinch and swipe events become `RawEvent::Gesture`. The scale since
    begin turns into a ratio per step. `RawGesture` gains `dx`/`dy`, the move of the fingers'
    middle; X11 and Wayland fill it too, and the pinch's focal point follows it. The tape format
    is now version 2. Video planes (ADR 0061): `Scanout::planes`/`show_plane`/`move_plane` on the
    CRTC's overlay planes. An NV12 plane imports a dma-buf or copies NV12 into dumb buffers; an
    XRGB-only plane gets a converted copy. Each new place is tried with `TEST_ONLY` first, and a
    frame with nothing to present waits for the refresh through `CRTC_QUEUE_SEQUENCE`. The facade
    moves a video that `Renderer::topmost_image` finds plainly on top onto a plane after upload,
    and back into its texture once it is covered. `Renderer::texture_rewritten` fixes in-place
    texture writes not redrawing under partial redraw. Also fixed: a crash at start on boards
    missing one of the dlopened libraries (libinput, libseat, libgbm), from `dlclose(NULL)`.
  - Verified: a fixture in `libinput debug-events` format (a pinch, a swipe, a cancelled pinch)
    replays through the gesture state machine in a Linux container. Videos tests with a fake
    display check a video going onto the plane and back when covered, and a refused place staying
    in the texture; renderer tests check `topmost_image` under clips and `texture_rewritten`. The
    board was vkms (`enable_overlay=1`) in a podman VM, running a CPU build with gstreamer. Uncovered,
    a 640x360 video sat on overlay plane 1 and its framebuffer changed ~30 times a second. The
    primary framebuffer did not change, so no UI frame was drawn, and the CRC showed 31 distinct
    frames a second. With a box over it, the plane came down and the video was composited again.
    Not verified: the NV12 and dma-buf paths, which vkms does not offer, and real hardware.

## N8 — Devices and verification

- [x] Run `01-counter` on one Embedded-GPU and one Embedded-CPU board, record the results into the
  footprint baseline, and compare CpuRaster output bit-exact with desktop emulation. Verify the
  §50A.5 budgets or record recalibrated ones.
  — Built:
  - `cargo xtask footprint --board <ssh>` (`--board-triple`, `--board-shell`, `--board-env`)
    cross-builds the embedded-cpu and embedded-gpu subjects and measures them on the board from
    `/proc`. Sizes are read from ELF section headers, and the baseline records the board.
  - `VISO_SCANOUT_CAPTURE` writes each presented CPU frame as a PNG, from the KMS output and from
    the macOS emulated panel.
  - An emulated panel's window no longer has a caption. Its scanout is in place before the view's
    first backing-scale report, so the frame keeps the panel's size instead of the backing's.
  - A GLES window falls back to linear when the driver refuses the sRGB colorspace (GBM on Mesa).
  Verified on a vkms aarch64 VM, 1024×768:
  - Counter, list and rgb565 counter frames are byte-identical PNGs on the board and in emulation.
  - CPU board: 6.88 MiB, first frame 30 ms, RSS 13.8 MiB, idle 0.
  - GPU board (llvmpipe): 7.41 MiB, 130 ms, heap 6.7 MiB, idle 0.
  - Budgets recalibrated: RSS is scaled to the panel, and the first-frame budgets are kept. The
    binary is over budget, which is the next item.
- [x] Link only what an app uses. A counter's binary carries every control kind, the game kit,
  scene3d, every image decoder (~480 KiB) and svg (~198 KiB), because views name widgets and
  natives at run time. The build already knows the compiled views, so it can tell the facade
  which control kinds, natives and asset formats they reach (the `VISO_GPU`-style build cfg).
  Also gate cumulative size growth: desktop counter went from 5.00 MiB to 8.91 MiB between 10-18
  and 10-28 while each paired check passed. Verify with §50A.5's binary budgets (embedded-cpu
  ≤ 4 MB, embedded-gpu ≤ 4.5 MB) on the board subjects, and that an app using a pruned control
  still builds and draws it. Built (§50A.5.2 按用链接):
  - The shipping profile uses fat LTO with one codegen unit, and the footprint xtask builds the
    embedded window and board subjects as shipping.
  - `view!` hands the host just the standard native libraries (`Natives::exactly`) and the
    control kinds (`viso_view::kinds`, one `Kind` static per kind, reached through function
    pointers) that it uses.
  - Game audio, glTF decode, animated-image playback and paged capture are wired in at the call
    that uses them.
  - A board with converted rasters and no SVG links neither decoder.
  - `PANEL_ONLY` folds the caption, menu bar and accessibility out of KMS-only builds.
  - `footprint --against` also holds sizes to `baseline.json` (`check_sizes`).
  Verified:
  - Board subjects: embedded-cpu 3,947,176 B and embedded-gpu 4,461,920 B (were 6.88 / 7.41
    MiB). Desktop counter is 8.45 MiB and embedded-cpu window 3.94 MiB.
  - The baseline was re-recorded.
  - Facade control tests pass on the linked-kinds path (non-hot-reload `view!`, with a
    `debug_assert` on unlinked kinds): slider, combo, search, picker, stepper, chips, segmented,
    swipe row, router, shaders and others.
  - `sizes_cannot_creep_past_the_baseline`.
- [x] Android chooses its GPU API at run time: `[target.android] gpu = "auto"` (the default)
  draws with Vulkan where the device opens it and with GLES 3 otherwise; `vulkan` / `gles` pin
  one (ADR 0062). GLES imports video without copying. Built: `AndroidBackend` (an enum, one
  `match` per RHI call) chosen once per process, with the launch variable `VISO_GPU` overriding.
  A build script turns the manifest key into `cfg(viso_gpu)`. `gles::external`: hardware
  buffers through `samplerExternalOES`, NV12 dma-bufs as `R8`/`GR88` EGL images, and NV12 at hand
  uploaded. Each holds its buffer until a fence signals. Also `Gles3Backend::read_texture`. Verified:
  - On the SwiftShader emulator the default build falls back to GLES and prints the reason. The
    video example's hardware buffers import (one import, then adopted every frame) and play with
    exact colors. The launch variable `VISO_GPU=vulkan` makes it panic with the reason instead
    of falling back. `gpu = "gles"` runs GLES and ignores that variable.
  - The GLES tests (`tests/gles_external.rs`, NV12 at hand and in a dma-buf) pass on Mesa.
  - Clippy is clean for auto, gles and vulkan on aarch64-linux-android.
  - Sizes on the reference app: auto 9.93 MB / 4.70 MB gzip, Vulkan alone 9.61 / 4.55, GLES
    alone 9.72 / 4.61.
- [ ] The 3D scene lane on real Vulkan, D3D12, GLES and WebGPU devices: today only the CPU raster
  and Metal are drawn and compared. Verify with the scene lane goldens on each.
  - [x] Vulkan, GLES and WebGPU. Built:
    - `crates/render/tests/devices` draws one picture on every device the machine has and reads
      it back: Metal; Vulkan (native, or the `vulkan` feature) with validation; GLES 3 (the new
      `gles` feature of viso-render); D3D12.
    - `scene3d_lane`'s lane pictures, its mirrored-skin culling, and `scene_model`'s glTF front
      views are each compared against the CPU raster on each device.
    - `tests/scene_pictures` shares the lane pictures with `webgpu_backend.rs`, which draws them
      on WebGPU.
    - `tools/deno2` runs wasm-bindgen-test-runner's Deno harness on Deno 2.
    Verified: every lane picture and glTF model matches the CPU raster within its tolerance on:
    - Metal and on Vulkan through MoltenVK (macOS);
    - Mesa lavapipe (Vulkan) and llvmpipe (GLES 3, surfaceless) in the `viso-gles` container;
    - Deno 2.9's WebGPU, which runs wgpu on Metal.
    The WebGPU suite passes, 7 tests.
  - [ ] D3D12: the device comparison builds for `x86_64-pc-windows-msvc` and runs on any
    Windows machine (`cargo test -p viso-render --test scene3d_lane --test scene_model`); none is
    at hand. Also a browser's WebGPU (Dawn in Chrome), which needs chromedriver.
- [ ] Windows: WASAPI audio playback and capture, `signtool` signing of the executable and the
  MSIX. Verify on a Windows machine.
- [ ] Apple: a Developer ID signature, notarized; a device APNs token. Verify on real
  credentials.
  - [x] Notarized. Built:
    - `[package.macos] notary_profile` (a `notarytool store-credentials` keychain profile), or
      an App Store Connect API key in `VISO_NOTARY_KEY`, `VISO_NOTARY_KEY_ID` and
      `VISO_NOTARY_ISSUER`.
    - The package flow: sign the `.app` → notarize and staple it → build the dmg → sign → notarize
      and staple the dmg.
    - `notarytool submit`, then `wait`, then `info` gives the status; anything but `Accepted`
      fails with the `log`.
    - `auto` warns on a signed but unnotarized app, or on an identity Apple does not notarize;
      `required` refuses both.
  - [x] A Developer ID provisioning profile for macOS. Built:
    - `[package.macos] provisioning_profile` is checked against `bundle_id` and embedded as
      `Contents/embedded.provisionprofile`.
    - The restricted entitlements the fragments ask for (push's
      `com.apple.developer.aps-environment`) are signed with the profile's values, plus the app
      and team identifiers. Without a profile they are left out, with a warning naming the key.
  - Verified: notarytool's answers parse; the notary arguments; resolution by profile, by API
    key, refused and short; the provisioned entitlements (allowed and refused).
  - [ ] On real credentials: `viso package macos --signing required` with a Developer ID
    identity and a notary profile, then `spctl -a -vv` and `stapler validate` on the `.app`
    and the dmg. A push token on an iOS device and on macOS under the profile.
- [ ] CI emulator and simulator jobs: boot, install, hot reload, and the examples rendering on the
  iOS simulator, the Android emulator and Chromium (open lines in `todo_hotreload.md` and
  `todo_platform.md`).
  - [x] The device e2e tests pass on this Mac: boot, install, hot patch, Rust warm restart with
    state restored, and the interrupt ending each session. Found on the way: the commands that
    stop an app (`simctl terminate`, `am force-stop`, `adb reverse --remove`) were themselves
    ended by the interrupt they served, so an interrupted simulator session waited forever on
    the app's link; they now run through it (`process::finish`, which the board's SSH calls
    share). `javac` and `keytool` now come from `JAVA_HOME` as the other JDK tools do.
  - [ ] The jobs' first run on CI, and the examples rendering there.
- [ ] The map on Android (emulator with an API 36 image, or a device) and touch gestures on iOS:
  pan, pinch, rotate and tilt against the desktop behavior.
  - [x] Android: `examples/map` has a `Viso.toml` and runs through `viso run android` on the API 37
    emulator (GLES, its Vulkan instance does not open). Its data was unreadable where `adb push`
    put it: nothing had made the app's external files directory, so the shell owned it. The
    new `viso::services::documents_dir` asks `getExternalFilesDir(null)`, which makes it the
    app's; on iOS it is the container's `Documents`. With the Luxembourg data pushed there,
    the map draws, and the emulator's gRPC touch input (two tracked fingers) pans it, pinches
    it about the fingers' midpoint, rotates it with them (labels stay upright) and tilts it with
    a two-finger slide up into the 3D buildings and the horizon, as on the desktop.
  - [x] iOS simulator: `viso run ios` launches it and, with the data copied into the
    container's `Documents` (`documents_dir`), the map draws.
  - [ ] iOS touch gestures: the simulator's window is Xcode's Device Hub
    (`Xcode.app/Contents/Applications/DeviceHub.app`), whose Option-drag gives two fingers;
    it needs an unlocked, on-screen session to take mouse events (`simctl` has no touch
    input), or a device. One-finger pans and taps work there, including after the map goes
    idle: the backend used to leave a pending frame (`ControlFlow::Poll`) to whatever asked
    for the next beat, so once tiles and animations settled, input changed state but no frame
    ran. Two fingers remain: synthetic Option comes through as a key while Device Hub
    captures input, so the pinch, rotate and tilt need a hand on the Mac or a device. A
    trackpad's pinch and rotation (an iPad's, and Device Hub's if it passes the Mac's on)
    now arrive as the trackpad gesture, as on the Mac. The Android emulator's Command-drag
    reports the second finger's landing at a stale point; a second finger whose first move
    leaps is now measured from where it moves, so the spread zooms in as it should. The
    emulator passes no trackpad pinch on at all.
- [ ] High-refresh frame pacing (120, 144, 240 Hz) on presenting devices. Verify with GPU-time and
  cadence traces.
  - [x] Nothing caps a frame at 60 Hz. Built:
    - iOS apps carry `CADisableMinimumFrameDurationOnPhone`, without which an iPhone's
      ProMotion stays at 60 whatever the display link asks; the link asks for the screen's
      `maximumFramesPerSecond`.
    - Android votes on its surface for the display's highest rate at its resolution
      (`Surface.setFrameRate`, API 30; API 31 only if seamless). It is a vote that counts only
      while frames arrive, so an idle app leaves the panel be.
    - macOS (`CAMetalLayer` display sync), Windows (DXGI flip) and Wayland present at the
      display's rate.
    - The loop measures its interval (`FramePace`); scroll, fling, spinners and transitions
      advance by measured time. Only the game's fixed step is 60 Hz, by design.
  - [ ] On devices: cadence traces at 120 Hz (iPhone Pro, an Android flagship) and at 144/240 Hz
    (a desktop panel).
- [ ] Device runs of what is built and only compile- or headless-checked (carried over by N9):
  - secure text entry on each platform (`todo_dsl.md`);
  - `@persist` storage in the iOS sandbox. Verified on Android (the API 37 emulator: three
    taps on `examples/persist`, home, force-stop, relaunch reads "3 in all, 0 this run") and
    on the web (`web-gpu` in headless Chrome: three clicks store `viso/tally.count`, a new
    page reads 3, as `viso inspect tree` on the session shows);
  - game audio output: WASAPI, ALSA, AAudio, iOS RemoteIO;
  - a live macOS session: cursors, IME candidate placement, appearance and key-window
    notifications, memory pressure (`todo_platform.md`);
  - GPU time on presenting Vulkan and D3D12 devices (`todo_cli.md` C6).
- [ ] ASUN rows decoded as they arrive in `viso-net`, once `asun-rs`'s incremental decoder lands.

## N9 — Governance

- [x] Move the implemented ADRs from Proposed to Accepted after review: 0034–0040, 0042–0044 and
  0047. Verify that the Part XXVIII table matches the files.
  - Reviewed each against the code, with 0054–0056 as well; all are Accepted, each with an
    "As built" list where the code differs from the record (codes, crate homes, sizes, what was
    later built). The table now lists 0061 and 0062 and the amendments.
  - Found on the way: `viso-ui-macros` depended on `viso-behavior` outside the DAG; it now
    reaches `standard_name` through `viso_dsl::schema` and `check-deps` passes. ADR 0044 says
    `check-profiles` validates every profile: it now also holds the mobile profile on iOS and
    Android and the web's on wasm32 (11 closures, all clean).
- [x] Sweep the older todo files (`todo_cli.md`, `todo_dsl.md`, `todo_hotreload.md`,
  `todo_rendering.md`) for lines done since, such as the `GameViewport`/3D lane lines finished in
  W1 and W14. Tick them or move what is left here.
  - Ticked: `GameViewport` and the World's drawing, the game frame sheet, video planes, SDR
    encoded once, `web-dom`; the Studio lines dropped (there is no Studio). The shader
    two-backends line now also runs on Linux: `shader_golden` with `vulkan` on lavapipe agrees
    with the CPU reference (worst channel difference 1).
  - The rest went to the Backlog at the end of this file, and the device runs of built code to
    N8; each older file says so at its top.

## N10 — Editor extensions (last)

There is no Studio: the CLI's hot reload, `viso inspect` and `viso profile` are the tools, and the
editor extensions are their interface. They live in their own repository, `viso-vscode`. One
repository builds two thin shells, VSCode and Zed, over the shared `viso-lsp`. They start after
everything above is done.

- [x] `.vs` language support for VSCode and Zed. Both register the language, give its comment and
  bracket rules and launch `viso-lsp`. VSCode highlights with a TextMate grammar and Zed with a
  Tree-sitter grammar and queries. Verify that each opens a fixture, highlights it and shows a
  diagnostic, hover, go-to-definition and formatting from the server.
  - [x] VSCode: the integration suite runs in an installed VSCode on `fixtures/app`. Zed: the
    extension builds for `wasm32-wasip2` and the Tree-sitter corpus and highlight tests pass on
    the same fixture, with Zed's `highlights.scm` checked as their copy.
  - [ ] Zed opened by hand on the fixture (*install dev extension*), on the next device run.
- [x] Semantic tokens and completion in `viso-lsp` (components, properties, events, states,
  natives, from the widget schema and the native registry). Verify with LSP tests, and that both
  editors use them.
  - [x] `viso-lsp` advertises both; its unit and protocol tests cover classes, declarations,
    natives through an import, contextual keywords, the UTF-16 encoding, and completion of
    properties, events, members, locals, library paths, types and keywords. The VSCode suite
    reads both through VSCode's own commands. Zed takes them through the same server.
- [x] The DSL inside `.rs`: injected highlighting for `ui! { … }` and `component! { … }`, and
  `view!("x.vs")` as a link to its file, where each editor allows it. Verify on a fixture crate.
  - [x] VSCode: the injection is checked by the grammar test on `fixtures/app/src/main.rs`, the
    link by the integration suite. Zed allows neither: an extension cannot inject into the
    built-in Rust language or add document links.
- [x] Optional: an inspect/profile panel over the CLI's JSON event stream in VSCode, and the
  language features inside a `ui!` body through a virtual document. Verify against a running
  `viso run`.
  - [x] Inspector, Events and Profile views run `viso inspect tree`, `viso inspect events` and
    `viso profile` with `--json`; rows open the source they name. `ui!`/`component!` bodies
    are mirrored to `.vs` documents for completion, hover and definition, mapped back.
  - [x] Verified: node unit tests on the stream and the mirrors; the VSCode suite fills the
    Inspector (17 nodes, a node opens `src/inbox.vs` at its line) and Profile views from a
    headless `examples/i18n` and completes and resolves inside the fixture's `component!`;
    against a running `viso run` of `examples/i18n` on macOS the same three commands answer,
    and a hot-reload edit during capture gives 44 frames and 6 `reload` dirty events, read as
    rows by the views' reader.


## Backlog — carried over from the older todo files

Open lines the N9 sweep found in `todo_cli.md`, `todo_dsl.md`, `todo_hotreload.md`,
`todo_rendering.md`, `todo_platform.md`, `todo_arch.md` and `todo_features.md` that are neither
done nor already an item above. Not scheduled; each names the file and section it came from.
Device-only verification of what is built (secure text entry, `@persist` paths, game audio
output, a live macOS session) is listed with the device runs of N8.

- [x] `Brush::ImagePattern` and `Brush::ShaderBrush` lowering in the renderer: both are declared
  and reach a `todo!` panic (`todo_rendering.md` D2.1). A compact shared stop table between the
  inline 2-stop and the 3+ stop LUT tiers. Each brush is a primitive family of its own: the
  `Brush`/`BrushStore` copy nothing fed was removed with its panics. A shader brush is a
  `ShaderView`'s brush program, already drawn; an image pattern is now `ImageFit::tile` (one
  repeat-sampled draw for a raster, a copy a cell clipped to the box for a vector; a repeating
  `background-image`, or mask under a tint, in the HTML exports). No compact stop table: a stop
  loop per pixel costs more than the LUT's one sample, and a LUT row bakes once.
- [x] `ResourceScope` beyond `component`: caches shared across instances (`todo_dsl.md` D7).
  `ResourceScope::shared` gives each declaration a pool in the view: a key loading is joined,
  not loaded again, its result settles every instance waiting on it, `keep_latest` cancels a
  load once none waits, and the pool's loads and expiries are the view root's, outliving the
  instance that started them. The VM host, the release package and both web exports run it.
- [ ] A component whose view root is a region (`view { match .. }`), mounted in region content,
  shows nothing and faults on its region entries: its states, effects and starters are claimed
  at its first node, and a region item has none to anchor them (found with the shared
  resources; wrapping the view in a `Column` works).
- [ ] The `renderer_steady_state` bench stops at its decorated-cards gate: 496 draws where it
  asserts 512 (two per card), so no bench of it runs.
- [x] The CLI's preview of generated code runs its views on the preview host, under its
  restricted grant (`todo_dsl.md`, `E6103`).
  `viso preview <file> [<component>] [--grant <capability>].. [--output <dir>]` compiles the
  file with the package, mounts its component alone in a headless run handed
  `VISO_HEADLESS_GRANT`, reports every fault the view raises as a diagnostic at its span
  (exit 5) and writes its `.png`/`.tree`.
- [ ] No host installs the `Clipboard` service the `viso::clipboard` natives read: the app's
  view host and the headless harness both answer `E7106` (found with `viso preview`, its
  `--grant clipboard.write` past `E6103`). The platform clipboard should back it.
- [x] A compile-fail test of a catalog error through `view!` (trybuild), and the dev session's
  catalog reload under a real watcher (`todo_dsl.md`, localization checks).
  `view_catalog_error` reads a fixture package's broken `i18n/en.toml` (a `Viso.toml` between
  a source file and the crate root is now the file's package); a dev session over a temporary
  project takes its watcher's deliveries, patches on a saved catalog and reports a broken one.
- [ ] Game audio on the web: an AudioWorklet over a shared-memory wasm build (`todo_dsl.md`,
  `AudioProcess`), which needs the threaded web build below.
- [x] `[profile.<name>.web] split` and `threads`: a startup/lazy module split and a
  shared-memory threaded module (`todo_cli.md` C4.2).
  - [x] `split`: profile-guided by a headless training run (`wasm-split`), the lazy part
    compiled in the background and loaded on the spot by a call that comes first. The
    map's shipping startup: 1.86 → 1.19 MiB brotli, 3.66 → 1.44 MiB of code to compile.
  - [x] `threads`: `on` builds a shared-memory module on nightly `-Z build-std`; the
    work pool's workers run on module Workers of the glue, a page-thread lock spins
    instead of waiting, and a worker's wake reaches the page as a message.
- [ ] The map's route card shows `…: the server answered 404` on one line past the card's
  edge in the frame it first appears, wrapped only later (found with the split; the
  unsplit bundle does the same).
- [ ] Per-target availability of natives, and the capability table rows it gives; lands with the
  first native missing on some target (`todo_cli.md` C1.5, C4.4).
- [x] A pixel golden of the frame-profile HUD (`todo_cli.md` C6). `the_hud_draws_as_its_golden`
  shapes a capture's line with the fixture font, draws the HUD over a grey window on the CPU
  rasterizer and compares `tests/golden/profile_hud.bgra8`.
- [x] Recorded acceptance scenarios: resize, DPI change and surface recreate, with GPU frame time
  per scenario (`todo_rendering.md` D0.4). `viso profile --drive resize|rescale|recreate`: the
  runtime posts itself the geometry or surface events a step each frame while it is sampled
  and puts the window back after. Recorded on macOS (Metal), `examples/images`, 120 frames:
  resize frame p50/p95 7.8/9.0 ms, GPU 0.14/0.20 ms; rescale 7.7/65 ms (a drawable of another
  size each frame), GPU 0.08/0.20 ms; recreate 0.9/1.6 ms, GPU 0.19/0.20 ms, nothing laid out
  or painted again and 48 bytes uploaded.
- [x] Surface draw keys and pass sums skip the hash work an unchanged entry already did — measured and declined: dropping the hash entirely leaves `upload_100k_steady` within noise, and caching each entry's inputs (56 bytes an entry) made it 5–20% slower; the reasoning is in `Viso_Rendering.md`
  (`todo_arch.md`).
- [ ] An opt-in disk cache of dev snapshots, once the DSL can mark a state sensitive
  (`todo_hotreload.md`).
- [ ] A published `viso` on a registry, so a project made without `--framework` resolves without
  a `[patch]` (`todo_cli.md` C1.2).
- [ ] Usability: timed sessions with people, and real-model prompt sets and generation runs
  (`todo_dsl.md` D9).
