# Viso Feature Completion — W1 → W16

The rendering, text, tooling, DSL and hot-reload programs are complete. This program fills
the remaining surface gaps:

- what makepad has and Viso lacks;
- what neither has but a production cross-platform UI framework (Flutter / SwiftUI /
  Compose level) is expected to have.

The order is by dependency and value. W1 finishes the game screen, W2–W9 are the
everyday UI surface, W10–W12 are media, network and platform services, and W13–W16 are
the large optional subsystems.

Rules:
- Each item lands with its tests: unit, headless snapshot / semantics / input replay
  where it is UI.
- Each item lands with docs (spec text or ADR) in the same commit as the code.
- Large optional functionality goes to `extras/` or `integrations/`, not the default
  widget graph (AGENTS 33).
- Network/TLS/codecs are adapters (AGENTS 3.7).

## W1 — 3D scene lane and `GameViewport` (ADR 0047)

- [ ] S1 RHI:
  - [x] `DepthState` / `CompareFunction` / `CullMode` on `PipelineDesc`;
  - [x] `DepthAttachment` (transient) on `RenderPass`;
  - [x] `Geometry::InstancedMesh`, the uniforms a bind-group uniform buffer;
  - [x] on Metal, Vulkan, D3D12, WebGPU, GLES 3 and CpuRaster (a per-target f32 depth
    buffer and a perspective-correct triangle rasterizer);
  - [ ] test: overlapping triangles resolve by depth on every backend that runs here.
- [ ] S2 Scene shader built-in (`BuiltinShader::Scene`, `VertexSource::PerVertexInstanced`):
  - [ ] vertices `pos/normal/uv` and instances `model[3]/color` read by index;
  - [ ] a camera/light uniform block;
  - [ ] Lambert + Blinn-Phong + hemispheric ambient, linear color;
  - [ ] MSL/WGSL/HLSL/GLSL codegen, frozen SPIR-V;
  - [ ] the CPU fill.
- [ ] S3 Render:
  - [ ] `Scene` resource (camera, light, clear, instance batches), `SceneMeshStore`
    with procedural primitives (cube, sphere, capsule, cylinder, cone, wedge, plane)
    and composite Kit models;
  - [ ] `PassWork::Scene` before the 2D pass that composites it;
  - [ ] frustum culling on fill;
  - [ ] targets sized to 64-px steps.
- [ ] S4 UI/view:
  - [ ] `Content::Scene { scene }` node content;
  - [ ] `GameViewport` (DSL + Rust) fed each frame from `GameWorld::extract`,
    `Stage::view()` and terrain;
  - [ ] PAINT only when the world, camera or size moved;
  - [ ] `camera:` override;
  - [ ] semantics role `image`.
- [ ] S5:
  - [ ] particles as billboards (blended, depth-tested, no write);
  - [ ] debug shapes as thin boxes;
  - [ ] viewport focus takes game keys (§106.2); pointer down focuses.
- [ ] S6 Headless frame sheet (`viso game … --sheet`) and golden scene images through
  CpuRaster.

## W2 — Text editing

- [ ] Multi-line `TextInput` (`multiline: true` / `TextArea`): newline insertion, wrapped
  layout, vertical caret motion keeping the goal x, internal scroll to the caret.
- [ ] Undo/redo history with edit grouping (typing runs, word boundaries, paste/cut as
  one), bounded memory, IME commit as one edit.
- [ ] Word / line / paragraph / page / document motion and deletion (platform key maps:
  Alt/Ctrl, Cmd), Shift-extension.
- [ ] Double-click word select, triple-click line/paragraph select, drag-extend by word.
- [ ] Select-all, caret blink (paused while typing, off under reduced motion),
  horizontal scroll of a single-line field.
- [ ] Input formatters: max length, numeric/decimal/integer filter, custom filter
  callback, `on_submit`.
- [ ] `NumberField` / scrub value input (drag to change, step, min/max, wheel).
- [ ] Selection handles and the edit menu on touch (copy/cut/paste/select all).

## W3 — Rich and selectable text

- [ ] Styled spans: `TextRequest` carries runs (font, size, weight, style, color,
  decoration, background, baseline shift); `RichText` widget and DSL `Text { spans }`.
- [ ] Links: hit-testing span ranges, hover cursor, `on_link`, keyboard focus and the
  `link` role.
- [ ] Selectable text (`selectable: true`) on `Text`/`RichText`: drag/double/triple
  select, copy, focus ring, semantics.
- [ ] `Markdown` widget (CommonMark + tables, task lists, strikethrough, code blocks with
  monospace, images, links), incremental re-parse for streaming text.
- [ ] `Html` widget for a safe inline/block subset (p, h1–h6, b/i/u/s, a, ul/ol/li,
  blockquote, pre/code, br, hr, img).

## W4 — Anchored overlays

- [ ] Anchored placement: side/align, flip on collision, shift into the window, size to
  the available space, arrow offset, re-place on scroll/resize.
- [ ] `Select` / `DropDown`: anchored list, type-ahead, arrow/Home/End/PageUp/PageDown,
  scroll to the selected row, `listbox` semantics.
- [ ] `ComboBox`: filter as you type, free text or closed set, `combobox` semantics.
- [ ] `Menu` / context menu: items, separators, checkable items, shortcuts text,
  submenus with hover intent, keyboard navigation, `on_context_menu` (right click / long
  press).
- [ ] `Tooltip`: hover delay, grace period between targets, touch long-press, keyboard
  focus show, `tooltip` semantics.
- [ ] `Popover` with arrow (callout).

## W5 — Scrolling

- [ ] Kinetic scrolling: velocity tracking, platform deceleration curve, flick-catch
  on press.
- [ ] Overscroll: bounce (iOS) / glow-stretch (Android) by platform style; rubber-band
  on drag.
- [ ] Scrollbars: overlay and classic, draggable thumb, track paging, auto-hide.
- [ ] Smooth `scroll_to` / `scroll_into_view` with animation; auto-tail (stick to end).
- [ ] Pull-to-refresh.
- [ ] Sticky section headers in `List`.
- [ ] Scroll snapping and a `Pager` / `Carousel` widget.
- [ ] 2D virtual grid (`LazyGrid`, fixed and masonry columns).

## W6 — Animation

- [ ] Easing set: all Penner curves (quad … bounce, in/out/in-out) and `cubic_bezier`.
- [ ] Springs: stiffness/damping/mass, interruption with velocity handoff, settle
  detection.
- [ ] Keyframes: multi-stop tracks, repeat count / infinite, reverse, alternate.
- [ ] Reduced motion: platform setting honored by transitions and springs.
- [ ] Navigation transitions: push/pop slide, fade, platform default; shared-element
  (hero) transition.
- [ ] Animated `Toggle` thumb, `Checkbox` check, `Disclosure` chevron.

## W7 — Gestures

- [ ] Double/triple tap (`on_double_tap`, `double_click` route).
- [ ] Swipe/flick recognizer with direction and velocity.
- [ ] Pinch/zoom and rotate recognizers (touch and trackpad magnify/rotate).
- [ ] Gesture arena: competing recognizers, claim and reject, nested scroll
  coordination.
- [ ] `InteractiveViewer` (pan/zoom container with bounds and inertia).
- [ ] Swipe-to-reveal row actions; drag-handle reorder in `List`.
- [ ] `Sheet` detents and drag-to-dismiss.

## W8 — Drag and drop

- [ ] In-app DnD: drag sources with typed payloads, drop targets with accept/hover/drop,
  drag preview, keyboard alternative.
- [ ] OS file/text drop into a window: macOS, Windows, X11 (XDND), Wayland, web, iOS
  (UIDropInteraction), Android.
- [ ] Drag out to the OS (files/text) on desktop.

## W9 — Widgets

- [ ] `ProgressBar` (determinate/indeterminate), `ProgressRing`, `Spinner`.
- [ ] `Stepper`, `SegmentedControl`, `Chip` (filter/input), `Badge`, `Avatar`,
  `LinkLabel`.
- [ ] `SearchField` with suggestions (built on W4 placement).
- [ ] Slider: vertical, range (two thumbs), step marks, wheel, live thumb; `Knob`
  (rotary).
- [ ] `ColorPicker` (HSV wheel/square, alpha, hex input).
- [ ] `DatePicker`, `TimePicker`, `Calendar` (locale first weekday, keyboard grid
  navigation).
- [ ] `DataGrid`: virtualized rows and columns, resize, reorder, sort, cell/row
  selection, keyboard navigation, `grid` semantics.
- [ ] Charts (in `extras/`): line, bar, area, scatter, sparkline.
- [ ] Tabs: overflow scroll, close buttons, reorder.
- [ ] `Splitter` live relayout.
- [ ] `Skeleton` / shimmer placeholder.
- [ ] Form: field validation, error text, `Form` submit, field groups.

## W10 — Media

- [ ] Image decoders: GIF (animated), WebP (lossy/lossless/animated), APNG, BMP, ICO,
  QOI.
- [ ] Runtime image loading: file path, bytes, URL (on W11); decode cache with memory
  budget, placeholder/error states, `on_load`.
- [ ] `AnimatedImage` playback (frame timing, loop count, pause off-screen).
- [ ] Video playback (`Video` widget): AVFoundation, Media Foundation, GStreamer,
  MediaCodec, HTML video; the external/YUV texture path in render.
- [ ] Camera capture service and `CameraView`.
- [ ] Audio: input (microphone), file decoding (WAV, Ogg Vorbis, MP3, FLAC), Web Audio
  output, PulseAudio/PipeWire output.

## W11 — Networking

- [ ] HTTP client service (platform-native: URLSession, WinHTTP, libcurl-free Rust adapter
  on Linux/Android, `fetch` on web): request/response, streaming body, cancel, timeouts,
  redirects.
- [ ] WebSocket service (native and web).
- [ ] `open_url` service.
- [ ] Network reachability.

## W12 — Platform extras

- [ ] Permissions: camera, microphone, location, photos.
- [ ] Location service.
- [ ] Gamepad backend (GameController, XInput/GameInput, evdev, web Gamepad API) into the
  game input.
- [ ] Rich clipboard: images and HTML.
- [ ] Tray / status-bar icon with menu.
- [ ] Deep links / URL schemes.
- [ ] Custom cursors and pointer lock.
- [ ] Fullscreen change events on every OS.
- [ ] Window state persistence.
- [ ] Dock/taskbar badge.

## W13 — Effects widgets

- [ ] `BlurView` / glass panel over the backdrop blur and material built-ins.
- [ ] Ink/ripple press feedback as a standard primitive; haptics wired into controls.

## W14 — 3D extended (after W1)

- [ ] glTF 2.0 loader (meshes, materials, textures, node hierarchy, animation) as an
  asset.
- [ ] PBR metallic-roughness material with image-based ambient.
- [ ] Shadow maps (cascaded for the directional light).
- [ ] Skinned meshes.
- [ ] SSAO and sky.

## W15 — Code editor widget (in `extras/`)

- [ ] `CodeEditor`:
  - [ ] rope document, multi-cursor, undo;
  - [ ] syntax highlighting through the `.vs` lexer and a TextMate-free token API;
  - [ ] line numbers, folding, wrapping, inlays and decorations, search/replace;
  - [ ] LSP client hookup.

## W16 — Missing in both

- [ ] Localization: ICU-style message formatting (plural/select), number/date formats.
- [ ] Printing (desktop) and PDF export of a view.
- [ ] Biometric authentication service.
- [ ] Push notification registration (APNs/FCM).
- [ ] Background tasks service.
