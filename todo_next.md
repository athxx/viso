# Viso next program — N1 → N9

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
- [ ] Follow-ups the new benches found:
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
  - A small component recompile costs about 1.7 ms of fixed cost (natives, prelude); profile it.
  - `JsonWriter` can't reuse its buffer, and `RollbackSession::receive` allocates two Vecs a
    packet.
  - `DrivenApp` has no damage accessor.

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

## N5 — Rendering, text, input and accessibility

- [ ] On the web without WebGPU, draw through CpuRaster into the canvas (`putImageData`) instead of
  leaving it blank. Verify in headless Chromium with WebGPU disabled against the headless golden.
- [ ] Controls take their default colors from the env `theme` instead of fixed dark-palette
  constants. Verify with snapshots under the light and dark themes, and that a theme swap marks
  PAINT only.
- [ ] Assistive-technology scroll actions on scroll viewports and virtual lists (`ScrollDown`,
  `ScrollUp`, `ScrollIntoView`, iOS `accessibilityScroll`). Verify with an AccessKit request test
  that reaches an unmounted row of a 10k-row list.
- [ ] Surrounding text and selection to platform IMEs:
  - macOS `attributedSubstringForProposedRange`/`selectedRange`;
  - iOS `UITextInput`;
  - Android `getTextBeforeCursor`;
  - Windows TSF/IMM reconversion;
  - Wayland `set_surrounding_text`.

  Verify with macOS Japanese reconversion and Android autocorrect.
- [ ] IME clause segments and the selected clause, drawn with a thicker underline on the active
  clause. Verify with headless preedit replay and a Japanese IME.
- [ ] Merge the labels of a node marked as one element (such as a list row) into one semantics
  node. Verify in the headless semantics tree.
- [ ] A wrapping Flow layout (`LayoutInput::Flow`, DSL `Wrap`). Verify with layout unit tests
  (gap, RTL, hidden children) and the differential layout.
- [ ] Typed `ShapedGlyph` cluster and glyph ids, and per-locale kinsoku for zh-Hans, zh-Hant, ja
  and ko. Verify with Text §29.1's open rows.
- [ ] Name `ReactiveCycle` (E4202) cells and effect nodes by state name and source span, routed to
  dev diagnostics. Verify with an E4202 sample in `viso dev`.
- [ ] One diagnostic shape shared by `viso-shader` and `viso-dsl`, in a leaf crate. Verify that the
  LSP and the CLI render shader diagnostics through one path.

## N6 — DSL and tooling

- [ ] Native widgets and traits in native package schemas, so an app's or an extension's Rust
  widget is usable from `.vs`. Verify with a fixture package widget that passes `viso check` and
  mounts in a `view!`.
- [ ] Move the CLI's `build/`, `dev/`, `device/`, `toolchain/` and inspect client into a shared
  tooling crate. Verify with `check-deps` that `viso-cli` is a thin consumer.
- [ ] Studio on the shared inspect and profile services, launched by `viso studio [--target]`.
  Verify that it builds without depending on anything the CLI does not.
- [ ] `web-dom` and `web-hybrid` runtime backends and the per-target capability table (the C4.3
  and C4.4 lines of `todo_cli.md`). Verify that `viso build web-dom` no longer reports `C0011` and
  `viso check web-dom` flags a GPU-only node.
- [ ] `#[derive(Encode, Decode)]` in `viso-macros` for plain messages. Verify by porting one
  dev-protocol message and asserting identical bytes.
- [ ] Extend the VM-vs-native differential test to every DSL doc-example fixture. Verify that it
  reports no mismatch.
- [ ] A `stable / preview / reserved` matrix per grammar production in `Viso_DSL_1.0.md`. Verify
  with a doc test that every production is listed.

## N7 — Widgets and extras

- [ ] `adaptive_split` (list-detail that collapses to a stack) and `adaptive_navigation` (bottom
  bar, rail or sidebar by size class). Verify with headless tests that resize across size classes
  and check the tree and the focus.
- [ ] A PDF viewer widget over `viso-pdf`'s reader side. Verify that a multi-page document renders
  and selects text in a golden test.
- [ ] A browser widget in `viso-ext/viso-webview`, which is still the cargo template. Verify that a
  page loads inside a node on macOS and Android.
- [ ] KMS: libinput gestures (pinch, swipe) as `RawEvent::Gesture`, and overlay planes for video.
  Verify with a libinput fixture, and a video scanned out on a board without the GLES pass.

## N8 — Devices and verification

- [ ] Run `01-counter` on one Embedded-GPU and one Embedded-CPU board, record the results into the
  footprint baseline, and compare CpuRaster output bit-exact with desktop emulation. Verify the
  §50A.5 budgets or record recalibrated ones.
- [ ] The 3D scene lane on real Vulkan, D3D12, GLES and WebGPU devices: today only the CPU raster
  and Metal are drawn and compared. Verify with the scene lane goldens on each.
- [ ] Windows: WASAPI audio playback and capture, `signtool` signing of the executable and the
  MSIX. Verify on a Windows machine.
- [ ] Apple: a Developer ID signature, notarized; a device APNs token. Verify on real
  credentials.
- [ ] CI emulator and simulator jobs: boot, install, hot reload, and the examples rendering on the
  iOS simulator, the Android emulator and Chromium (open lines in `todo_hotreload.md` and
  `todo_platform.md`).
- [ ] The map on Android (emulator with an API 36 image, or a device) and touch gestures on iOS:
  pan, pinch, rotate and tilt against the desktop behavior.
- [ ] High-refresh frame pacing (120, 144, 240 Hz) on presenting devices. Verify with GPU-time and
  cadence traces.
- [ ] ASUN rows decoded as they arrive in `viso-net`, once `asun-rs`'s incremental decoder lands.

## N9 — Governance

- [ ] Move the implemented ADRs from Proposed to Accepted after review: 0034–0040, 0042–0044 and
  0047. Verify that the Part XXVIII table matches the files.
- [ ] Sweep the older todo files (`todo_cli.md`, `todo_dsl.md`, `todo_hotreload.md`,
  `todo_rendering.md`) for lines done since, such as the `GameViewport`/3D lane lines finished in
  W1 and W14. Tick them or move what is left here.
