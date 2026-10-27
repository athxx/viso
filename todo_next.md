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
- [ ] Split `crates/viso/src/lib.rs` (5.6k lines: `AppDriver`, per-window state, frame phases,
  access keys) into modules, leaving docs, `mod` and `pub use`. Verify that the `cargo doc` item list
  is unchanged and that the facade tests pass.
- [ ] Remove dead surface:
  - the marker contexts `LayoutCx`/`PaintCx`/`RenderCx`/`TaskCx`;
  - `viso_ui::token` (`TokenId`/`TokenInterner`/its `Theme`) with `StyleId`/`resolve_styles`/
    `set_style_token`;
  - `TranslateAnim`/`AnimationRegistry`/`EventCx::request_animation` (port
    `tests/animation_loop.rs` to `animate`);
  - `WireId`/`IdKind`;
  - the `mobile` feature.

  Verify with `cargo check --workspace --all-targets`.
- [ ] Produce or remove `RedrawReason::TimerDue`/`HotReload` and
  `FrameDecision::BackgroundMaintenance`. Verify with scheduler unit tests.
- [ ] `viso::version()`. Verify that it matches `CARGO_PKG_VERSION`.

## N3 — Robustness and security

- [ ] Bounded collection reads in `viso-ende`: `Decoder::read_count(max)` checks the cap and
  `count <= remaining()`. Replace the three hand copies in `schema/src/codec.rs`,
  `behavior/src/wire.rs` and `dev-protocol/src/wire.rs`. Verify that a huge count over zero-byte
  items fails fast.
- [ ] cargo-fuzz targets for `GameNet::receive` packets, AOT and view packages, behavior modules,
  the raster, SVG, glTF and font decoders, and text segmentation. Verify that each runs 10 minutes
  in `fuzz.yml` without a crash, with `encode(decode(x))` stable.
- [ ] Record `FINGERPRINT_VERSION` in persisted artifacts (view package, persist store) and refuse
  a mismatch with a diagnostic. Verify with a test that bumps the version and gets the error
  instead of silently dropped state.
- [ ] Recreate the GPU device after a loss (D3D12 removed or reset, `VK_ERROR_DEVICE_LOST`,
  WebGPU `device.lost`) and re-upload from the asset tables. Verify by forcing a removal and
  checking that the next frame redraws without a panic.
- [ ] Fixed-capacity `NodeStore` under the embedded profiles, sized from the footprint profile; a
  full store reports `ArenaFull` instead of panicking. Verify with an embedded-cpu headless test
  that overfills it.
- [ ] `ProtocolTag` bad magic returns `DecodeError::Malformed { offset }`. Ende golden-byte tests
  run on a 32-bit target. Verify both in CI.
- [ ] A debug-only owner-store tag on `NodeId`, with an assertion on cross-window use. Verify with
  a debug test that uses window A's handle on window B.

## N4 — Performance

- [ ] Pack the per-node bool columns (`hittable`, `hidden`, `clips`, `overlay`, `chrome`,
  `focusable`, `disabled`, `transform_root`) into one `NodeFlags` column. Verify with
  `node_arena`/`hover_diff` benches and a per-node bytes assertion.
- [ ] Move rarely set dense `Option` columns (`semantics`, `semantic_state`, `check_marks`,
  `interaction`) to sparse storage. Verify that column bytes drop and `semantic_projection` does not
  regress.
- [ ] Derive semantics per dirty subtree: keep the last tree and diff only re-derived branches.
  Verify on 10k nodes that one label change re-derives O(depth) nodes and that the delta equals a
  full re-derive's.
- [ ] Skip re-measuring clean subtrees inside a relayout root. Verify with the incremental-vs-full
  differential test and a 2000-row Column bench.
- [ ] Drive glyph representation promotion at runtime: MTSDF for runs under sustained scale or
  rotation, settling back to A8, with real MTSDF pages. Verify with:
  - a scaled/rotated text golden;
  - counters that stay 0 for pure translation;
  - Text §29.1's MTSDF rows.
- [ ] Reorder-safe spans in the batch planner, so text over sibling cards rejoins one glyph draw.
  Verify with planner order tests and a draw-count drop in `paint_scale`.
- [ ] `Encoder::clear` and `Encoder::from_vec`; reuse one encoder for game net packets and dev
  frames. Verify that a counting allocator sees 0 allocations per steady-state packet.
- [ ] Derive the inline work slice from the measured frame interval and share it across windows.
  Verify at 120 Hz emulation that the slice holds with N windows.
- [ ] Cancel a freed node's tasks through a per-node chain instead of scanning every task. Verify
  with a bench that frees 1k rows with 100 live tasks.
- [ ] Rust fragment functions generated at build time from Shader IR for CpuRaster user programs.
  Verify that they are bit-exact against `CpuShader` and faster in the `cpu_raster` bench.
- [ ] The missing benches:
  - `layout_10k_dirty_{1pct,100pct}`, `hit_test_100k`, `state_update_1k_bindings`;
  - `startup_*`, `hot_reload_*`, `ende_*`, `identity_*`;
  - `damage_single_label_update`, `memory_10k_nodes`;
  - widget microbenches after Tier 4 (DataGrid, Calendar, ColorPicker, DatePicker, NumberField).

  Verify that `cargo bench -- --list` shows each one.
- [ ] Hot, warm and cold bytes per node in `viso inspect frame`. Verify that the counter app is
  ≤ 64 hot bytes per node.

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
