# Viso Platforms — P0 → P11

Tier-1 (AGENTS 65.1): macOS/iOS → Metal, Windows → D3D12, Linux → Vulkan, Android → Vulkan,
Web → WebGPU. Today only macOS is real. P0–P11 run in order; each section closes one gap on
every target it names and is ticked only when `cargo check --target` is green for each of them
and the runtime path is verified where this machine can run it.

## What the audit found

| Layer | macOS | iOS | Windows | Linux | Android | Web |
|---|---|---|---|---|---|---|
| GPU backend | Metal | Metal (compile-checked) | headless fallback | headless fallback | headless fallback | headless fallback |
| Shader code | MSL | MSL | none | none | none | none |
| Window + loop | AppKit | none | Win32 (open/close/resize) | X11 (open/close/resize) | routed to X11 by `cfg(unix)` | none |
| Pointer / key / scroll / text / IME | yes | — | none | none | — | — |
| Scale factor / DPI change | yes | — | fixed 1.0 | fixed 1.0 | — | — |
| Menus / chrome geometry / fullscreen | yes | — | none | none | — | — |
| Clipboard / cursor / appearance | none | none | none | none | none | none |
| Touch / lifecycle / safe area / soft keyboard | vocabulary missing everywhere |||||
| Accessibility OS bridge | none | none | none | none | none | none |
| System fonts | CoreText | none | none | none | none | none |
| Color emoji raster | CoreText | none | none | none | none | none |
| Packaging | host binary | none | host binary | host binary | none | none |

Bodies of every built-in shader are verbatim MSL fragments (`ir/module.rs`), so no other
shading language can be produced from the IR as it stands. `RawWindowHandle` has no UIKit,
Android, Wayland or canvas variant, and its `Xlib` variant lacks the display pointer a Vulkan
surface needs. `viso-services` is a contract-only skeleton on every OS.

Verification reachable from this machine: Metal (host), iOS simulator (Metal), Android
emulator (arm64, Vulkan), Linux in a podman VM (Xvfb + weston + Mesa lavapipe Vulkan), Chromium
WebGPU through gstack `/browse`. Windows/D3D12 is `cargo check`/`clippy` only.

---

## P0 — Target routing and platform vocabulary

- [x] `viso-gpu`: `Backend` alias per target — `target_vendor = "apple"` → Metal (macOS
      `NSView` and iOS `UIView` surfaces); every other target resolves to `HeadlessRaster`
      until its backend lands in P2 (`windows` → D3D12, `linux`/`android` → Vulkan,
      `wasm32` → WebGPU).
- [x] `viso-platform` routing: macOS, Windows, Linux/BSD (X11) compiled; Android no longer
      falls into X11; iOS/Android/Web return `NoBackend` until P5–P7; Wayland runtime pick
      is P4. No `cfg(unix)` catch-all.
- [x] `RawWindowHandle`: `UiKit { ui_view }`, `Xlib { display, window }`,
      `Wayland { display, surface }`, `AndroidNdk { a_native_window }`,
      `WebCanvas { canvas_id }` (object id into the JS heap).
- [x] Event vocabulary: pointer id + kind (mouse/touch/pen) + pressure, `Cancel` phase;
      full `KeyCode` map (letters, digits, F-keys, modifiers, punctuation, numpad, media);
      `WindowFocus`, `AppearanceChanged`, `SafeAreaChanged`, `KeyboardInsetChanged`,
      `Suspended`/`Resumed`/`LowMemory` lifecycle events; `CopyRequested`/`Paste`.
- [x] `PlatformApp` hooks: `clipboard_text`/`set_clipboard_text`, `set_cursor`,
      `set_ime_area` (caret rect for candidate windows), `show_soft_keyboard`,
      `appearance()`; headless implements all of them deterministically.
- [x] macOS implements every new hook (NSPasteboard, NSCursor, `firstRectForCharacterRange`,
      `effectiveAppearance` + KVO, window key/resign).
- [x] Facade: the scheduler runs as a leaked `'static` on targets whose loop cannot block
      (Web), unchanged elsewhere. Facade routes copy/cut/paste (single-line fold), IME area
      + soft keyboard from the focused text node, one primary touch pointer, `Cancel`
      releases capture, `LowMemory` trims idle transient targets, `Resumed` redraws.

Follow-ups carried forward:

- [ ] Verify on a live macOS session: cursor switching, IME candidate placement,
      appearance KVO, key/resign notifications (compiled + headless-tested only).
- [ ] macOS never emits `LowMemory`; `Suspended`/`Resumed` map to app hide/unhide.
- [ ] iOS Metal `UIView` surface is compile-checked only; run it on the simulator in P2/P5.
- [ ] `std::time::Instant::now` panics on `wasm32-unknown-unknown` (`WallClock`) — P7.
- [ ] Multi-pointer routing + gesture arbitration (secondary touches are dropped today).
- [ ] `LowMemory` also trims the glyph/MTSDF atlases.
- [ ] IME area is the text control's box; switch to the caret rect once caret geometry is
      exposed.
- [ ] A cut does not fire `TextInput::on_change`.

## P1 — Portable shader bodies

- [x] A typed body AST for the built-in shading subset (decls, assignment, `if`/`else`,
      `switch`, `for`, ternary, calls, swizzles, member/index access, `u`/float literals),
      parsed from the existing body fragments; parse + type errors carry spans.
- [x] MSL stays byte-equal to the frozen oracles (bodies still spliced verbatim for Metal;
      the AST is checked to round-trip every built-in).
- [x] WGSL emitter: instance/vertex data as `@location` vertex attributes with instance step
      mode, uniforms at `@group(0) @binding(0)` (web) or `var<immediate>` (native → push
      constants), textures/sampler at group 1 (web) or 0 (native), reserved-word renaming,
      `select` for ternaries, explicit casts, `dpdx`/`dpdy`.
- [x] Texture reads lower to explicit LOD 0 (`textureSampleLevel` / `SampleLevel`): every
      texture is single-mip, and explicit LOD is legal in non-uniform control flow (blur loop).
- [x] HLSL (SM 5.1) from our own emitter: `ATTR<i>` semantics, `ConstantBuffer` at `b0`
      (root constants), `t0`/`t1`/`s0`; every built-in compiles through glslang's HLSL front
      end and passes `spirv-val`.
- [x] SPIR-V frozen as `crates/shader/spirv/*.spv`, generated from the native WGSL by `naga`
      (host dev-dependency only — no shader compiler ships); byte-equality test against a
      fresh compile (`VISO_BLESS_SPIRV=1` regenerates); `spirv-val --target-env vulkan1.0`.
- [x] `PipelineDesc` carries `ShaderCode` (MSL / WGSL / SPIR-V / HLSL); each backend states its
      `GpuBackend::SHADER_LANG` and the renderer asks the manifest for that language only;
      headless takes `ShaderCode::None`.
- [x] Every built-in's WGSL (web + native) passes `naga` validation.
- [ ] Semantic cross-check on real hardware: WGSL → MSL via `naga`, rendered on the host
      Metal device, pixel-compared with the native MSL pipeline for every golden scene.

## P2 — GPU backends

- [ ] iOS Metal: shared Metal backend over `CAMetalLayer` hosted in a `UIView`.
- [ ] WebGPU (`web-sys`): adapter/device async bring-up, canvas context, buffers, textures,
      samplers, pipelines, bind groups, render passes, offscreen targets, present,
      device-lost.
- [x] Vulkan (`ash`): instance/device selection, swapchain (Xlib, Wayland, Android),
      frames-in-flight ring, staging uploads, pipelines, descriptor sets, render passes,
      offscreen targets, recreate on resize/out-of-date, device-lost. Device tests run on
      MoltenVK here (`--features vulkan`); real Linux/Android swapchains unverified.
- [ ] D3D12 (`windows`): device, DXGI flip-model swapchain, command allocators per frame,
      fence ring, upload heap, root signature, PSOs, descriptor heaps, RTV/SRV, offscreen
      targets, resize, device-removed.
- [ ] Each backend implements the full `GpuBackend` trait, `Caps`, color space, and passes
      the same golden scenes as headless where it can run here.

## P3 — Windows platform parity

- [ ] Per-monitor-v2 DPI, `WM_DPICHANGED`, scale factor.
- [ ] Mouse (+ capture, leave tracking), wheel/hwheel, pointer/touch/pen (`WM_POINTER*`).
- [ ] Keys (`WM_KEYDOWN`/`WM_SYSKEYDOWN` → `KeyCode`), `WM_CHAR` text with surrogate pairs.
- [ ] IME (`WM_IME_*`, IMM32 composition string + caret, candidate window at `set_ime_area`).
- [ ] Clipboard (`CF_UNICODETEXT`), cursors, dark mode (`AppsUseLightTheme` + immersive dark
      title bar), focus, fullscreen, title, menus (`HMENU` + accelerators), timers/wakeup.

## P4 — Linux platform parity

- [ ] X11: XInput2 pointer/touch, XKB keymap + compose, XIM IME (preedit callbacks, spot
      location), clipboard (`CLIPBOARD`/`UTF8_STRING` selection ownership), cursors, Xft.dpi +
      RandR scale, focus, fullscreen (`_NET_WM_STATE`), title, wakeup pipe.
- [ ] Wayland (`wayland-client` + protocols): `xdg-shell`, `wl_seat` pointer/keyboard/touch,
      `xkbcommon` keymap, `text-input-v3` IME, `wl_data_device` clipboard,
      `cursor-shape-v1`/theme cursors, `fractional-scale-v1` + `viewporter`,
      `xdg-decoration`, frame callbacks.
- [ ] Runtime selection: Wayland when `WAYLAND_DISPLAY` is set, X11 otherwise.
- [ ] Appearance via the `org.freedesktop.appearance` portal setting.

## P5 — iOS platform

- [ ] `UIApplicationMain` + delegate, one `UIWindow`/`UIViewController`/Metal-backed `UIView`,
      `CADisplayLink` frames.
- [ ] Multi-touch → pointer events; hardware keyboard (`pressesBegan`).
- [ ] `UITextInput` for IME + soft keyboard, marked text → preedit.
- [ ] Safe area, keyboard frame, rotation/resize, trait-collection appearance,
      background/foreground lifecycle, `UIPasteboard`.

## P6 — Android platform

- [ ] Java `Activity` + `SurfaceView` shell with an `InputConnection` for IME; JNI bridge into
      a Rust loop thread fed by a message queue.
- [ ] `ANativeWindow` surface lifecycle (created/changed/destroyed), Choreographer frames.
- [ ] Touch (multi-pointer), key events → `KeyCode`, composing text → preedit.
- [ ] Insets (system bars, cutout, IME), density, dark mode (`uiMode`), lifecycle,
      `ClipboardManager`, pointer icons.

## P7 — Web platform

- [ ] `wasm-bindgen` + `web-sys`: canvas sizing with `devicePixelRatio` +
      `ResizeObserver`, `requestAnimationFrame` loop.
- [ ] Pointer events (mouse/touch/pen), wheel, keyboard, a hidden `<textarea>` for
      composition/IME + soft keyboard, clipboard (async API), CSS cursor,
      `prefers-color-scheme`, `visibilitychange` lifecycle.

## P8 — Text per platform

- [ ] Portable color-glyph rasterizer from the font file itself: COLRv0/v1 (Windows Segoe UI
      Emoji), CBDT/CBLC (Noto Color Emoji on Linux/Android), sbix (Apple), so emoji render
      identically without an OS raster.
- [ ] System font providers behind the existing `ExternalFontProvider` contract:
      DirectWrite (Windows), fontconfig (Linux), `/system/fonts` + `fonts.xml` (Android),
      CoreText (iOS, shared with macOS), bundled fallback (Web).
- [ ] Platform-default UI/monospace/emoji families and CJK fallback chains per OS.
- [ ] The macOS-only `text_content` tests run on every host that has a provider.

## P9 — Run, package, verify, document

- [ ] `cargo xtask bundle --target {ios-sim,ios,android,web}`: iOS `.app` (Info.plist,
      codesign ad-hoc for simulator), Android APK (javac → d8 → aapt2 → zipalign → apksigner,
      debug keystore), web bundle (`wasm-bindgen` output + loader HTML).
- [ ] Examples launch and render on: macOS, iOS simulator, Android emulator, Chromium
      WebGPU, Linux X11 + Wayland (podman VM, lavapipe).
- [ ] `cargo check --target` + `clippy -D warnings` for all eight installed targets.
- [ ] ADR: GPU backend static selection per target (the code cites "ADR-007", which is the
      scroll-viewport ADR); architecture document backend matrix updated.

## P10 — Accessibility OS bridges

- [ ] Semantics tree → NSAccessibility (macOS), UIAccessibility (iOS), UI Automation
      (Windows), AT-SPI over D-Bus (Linux), `AccessibilityNodeProvider` (Android), ARIA DOM
      mirror (Web); incremental updates, focus and actions routed back as events.

## P11 — Services per platform

- [ ] File open/save dialogs, share, notifications, permissions, secure storage, haptics —
      each a `viso-services` protocol with a headless mock and one implementation per OS.
