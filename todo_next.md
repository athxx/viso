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
- [ ] Run `check-profiles`, `check-targets` and `footprint --check` (against
  `benches/footprint/baseline.json`, with a tolerance) in CI. Verify that a seeded regression fails
  the job: `accesskit` added to the `kms` closure, or +10% heap or binary size.
- [ ] Add a benchmark regression gate beyond `check-frame-tax`: medians of `frame_loop`,
  `list_scroll`, `paint_scale` and `reactive_binding` against a stored baseline. Verify that a 20%
  slowdown injected into layout fails it.
- [ ] A first-party module-level forbidden-import scanner for platform, ui, render, gpu and dsl,
  run by `check-deps`. Verify that a planted `use viso_widgets` in `viso-ui` fails it.
- [ ] Tighten the allowlist to the manifests: the facade's `viso-dsl` and `viso-dev-protocol` move
  to `dev`; drop the facade's `viso-handle` and `viso-schema` and the services' `viso-runtime`.
  Verify that `check-deps` stays OK and that a normal `viso-dsl` dependency in the facade fails it.
- [ ] Extend the duplicate-geometry names in `check-deps` (`Size`, `Insets`, `Aabb`, `Ray`,
  `Plane`, `Transform3`), and rename `viso_ui::layout::Size` (a length request) and
  `viso_map::camera::Insets` (f64). Verify that `check-deps` passes with the longer list.
- [ ] `#![forbid(unsafe_code)]` on viso-ui, viso-widgets, viso-runtime, viso-schema, viso-dsl,
  viso-view, viso-lexer and viso-lsp; clippy `undocumented_unsafe_blocks` in CI for the rest. Verify
  that clippy passes once the ~12 blocks missing `SAFETY:` are documented, mostly in
  `crates/platform/src/backend/macos.rs`.
- [ ] Decide `libs/base64` and `libs/lz4`: make each a member with a user, move it to `vendor/`,
  or delete it. Verify that the workspace member list and `check-deps` agree.

## N2 — Public API

- [ ] Control builders return their typed handle: `build(cx) -> Handle<T>` (`Copy`, id only),
  replacing the `*HandleSlot` (`Rc<RefCell<Option<H>>>`) idiom, about 50 uses. Verify that every
  widget test that used a slot compiles against the returned handle, and that the
  `ModalHandle`-style methods still act through `EventCx`.
- [ ] A typed Rust state handle `State<T>` over `StateId` for the `StateValue` kinds
  (`cx.state_of(0i32)`, typed `get`/`set`). Verify that `examples/01-counter` has no `StateValue`
  match.
- [ ] `Application::on_lifecycle(&mut self, cx: &mut EventCx<'_>, Lifecycle)`, with `Lifecycle`
  re-exported. It is called on the launch window's context in this order:
  - on Suspended, before the persist flush;
  - on Resumed, after the redraw request;
  - on LowMemory, after the trim.

  Verify with a headless test that scripts the three events, checks the order, and checks that a
  `@persist` state written in the Suspended hook is durable.
- [ ] On Suspended, pause the playing videos and the game's audio output; on Resumed, resume only
  those the framework paused. Verify with the scripted player and a fake audio output.
- [ ] Make game sessions' `@persist` states durable on Suspended, through a store suspend hook.
  Verify with a `game_persist` test that suspends without dropping the session.
- [ ] Typed routes: a Rust `Route` enum, `Navigator<R>`, `RouterView` and `routes!` (DSL U11),
  serving `NavigationStack` and `on_open_url`. Verify with a DSL test and a UI scenario that push,
  pop, system back and a deep link keep the back-stack page's state.
- [ ] Typed Rust access to named nodes from `view!`/`component!` (`nodes.save_button:
  Handle<Button>`). Verify with a trybuild test that a wrong-type use fails to compile.
- [ ] `ui!` mounts any `viso::ui::Component`, so hand-written widgets and `component!` structs have
  one shape. Verify with a `ui!` case naming a hand-written `impl Component`.
- [ ] Add `Component`, `Size`, `Align`, `Justify`, `Axis`, `Inset`, `BoxStyle` and `Rgba` to the
  prelude, with `component!`/`view!` re-exported from it. Verify that the examples build with
  `use viso::prelude::*` as their only `viso` import.
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
