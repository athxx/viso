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

## W0 — Found on the way

- [x] Output color encoding: every backend writes linear-light values to a unorm surface
  with no transfer function, so author colors show darker than their sRGB value
  (`#1565c0` lands as `#022186`); encode exactly once (Rendering §5.4), sRGB images
  included, and re-bless the snapshot goldens (ADR 0048).
- [ ] A surface with no sRGB format (a GLES 565 panel, EGL without
  `EGL_KHR_gl_colorspace`) still draws unencoded: encode in a final composite pass.

## W1 — 3D scene lane and `GameViewport` (ADR 0047)

- [x] S1 RHI:
  - [x] `DepthState` / `CompareFunction` / `CullMode` on `PipelineDesc`;
  - [x] `DepthAttachment` (transient) on `RenderPass`;
  - [x] `Geometry::InstancedMesh`, the uniforms a bind-group uniform buffer;
  - [x] on Metal, Vulkan, D3D12, WebGPU, GLES 3 and CpuRaster (a per-target f32 depth
    buffer and a perspective-correct triangle rasterizer);
  - [x] test: overlapping instances resolve by depth (CpuRaster), and one scene draws the
    same on CpuRaster, Metal MSL and Metal through the naga-translated web WGSL.
- [x] S2 Scene shader built-in (`BuiltinShader::Scene`, `VertexSource::PerVertexInstanced`):
  - [x] vertices `pos/normal/uv` and instances `model[3]/color` read by index;
  - [x] a camera/light uniform block (a uniform buffer, read by both stages);
  - [x] Lambert + Blinn-Phong + hemispheric ambient + fog, linear color;
  - [x] MSL/WGSL/HLSL/GLSL codegen, frozen SPIR-V, `cross` in the body subset;
  - [x] the CPU fill.
- [x] S3 Render:
  - [x] `SceneFrame` (camera with perspective or orthographic projection, lighting,
    background, one instance batch per mesh) handed to a `SceneSlot` by
    `Renderer::swap_scene`;
  - [x] meshes keyed by `MeshId`: procedural cube, sphere, capsule, cylinder, cone,
    wedge and plane, plus uploaded user meshes;
  - [x] `Primitive::Scene` lowered to an image of the slot's target. The image's
    content revision is part of its damage key, and it patches in place;
  - [x] scene passes ahead of the 2D passes, only when the frame or size changed;
  - [x] frustum culling on fill;
  - [x] targets sized in 64-px steps, the projection fitted to the used part;
  - [x] CpuRaster: a scene pass clears what its target held;
  - [x] tests: composite, render-on-change, damage, patch, culling, resize steps,
    and Metal against CpuRaster.
- [x] S4 UI/view:
  - [x] `Content::Scene { slot, revision }` node content and the store's `SceneSlots`
    (commit → `PAINT` + revision; the facade swaps committed frames into the renderer
    and releases left slots);
  - [x] `GameViewport` (DSL + Rust `game_viewport`) fed each frame from
    `GameWorld::extract` (terrain included) and `Stage::view()`, with composite Kit models;
  - [x] PAINT only when the world, camera or size moved;
  - [x] `camera:` override (its own eased rig);
  - [x] semantics role `image`;
  - [x] tests: slot commit/sync/release, fills only under the owner, a viewport draws its
    game and redraws only on change.
- [x] S5:
  - [x] particles as billboards (blended, depth-tested, no write; sorted back to front);
  - [x] debug shapes as thin boxes (unlit material);
  - [x] viewport focus takes game keys (§106.2); pointer down focuses.
- [x] S6 Headless frame sheet (`viso test game … --sheet`, a `GameSheet` request run by the
  headless build) and golden scene images through CpuRaster; sub-pixel sprites keep a
  1.5-px radius at area-scaled opacity.

## W2 — Text editing

- [x] E1 Editing commands, shared by the widget and the DSL (`viso_ui::field_input`):
  - [x] word / line / paragraph / page / document motion, Up/Down keeping the goal x,
    Shift-extension;
  - [x] deletion by grapheme, word and line;
  - [x] platform key maps (Apple: Option word, Command line/document, the Emacs keys;
    elsewhere: Control word, Control Home/End, Control Y), select-all, undo/redo keys;
  - [x] undo/redo history: typing runs broken at word starts, Backspace and Delete runs,
    paste/cut/word and line deletion/IME composition/outside value as one step each,
    the selection restored, bounded to 256 KiB and 1000 steps, none for secure fields;
  - [x] paste as its own text event (`ImeEvent::Paste`), cut as one edit.
- [x] E2 Multi-line `TextInput` (`multiline: true`): newline insertion, wrapped layout,
  the accelerator with Enter submits, breaks unified to `\n`; DSL prop (HTML export as
  `<textarea>`) and widget setter.
- [x] E3 Field painting: caret, selection highlight and composition underline drawn by
  the framework; internal scroll to the caret (horizontal single-line, vertical
  multi-line) with the content clipped, wheel/pan scrolling inside the text; caret
  blink paused while typing, steady after idle and under reduced motion, hidden in an
  inactive window.
- [x] E4 Double-click word select, triple-click paragraph select, drag-extend by word
  or paragraph (click counting in the pointer router, `EventCx::clicks`).
- [x] E5 Input formatters: max length, integer/decimal filter, custom filter, widget
  `on_submit`; DSL props.
- [x] E6 `NumberField` / scrub value input (drag to change, step, min/max, wheel, arrow
  keys, typed entry), widget and DSL.
- [x] E7 Touch: long-press word select, selection handles, the edit menu (copy, cut,
  paste, select all).
- [x] E8 Pointer cursors per node: a node's hover cursor (DSL `cursor`), the I-beam
  over text fields, a resize cursor over a number field's scrub and a splitter.
- [x] E9 The edit menu where the OS has no touch one: a text field's context menu on
  right click (macOS `NSMenu`, Windows popup menu), a framework-drawn menu on Web, X11
  and Wayland; the Apple loupe while a long press carries the caret.

## W3 — Rich and selectable text

- [x] Styled spans: `TextRequest` carries runs (font, size, weight, style, color,
  decoration, background, baseline shift); `rich_text` widget.
- [x] DSL `Text { spans }` (a `Span` record), its HTML/script export, and `font_style`.


- [x] Links: hit-testing span ranges, hover cursor, `on_link`, keyboard focus and the
  `link` role.
- [x] Selectable text (`selectable: true`) on `Text`/`RichText`: drag/double/triple
  select, copy, focus ring, semantics.
- [x] `Markdown` widget (CommonMark + tables, task lists, strikethrough, code blocks with
  monospace, images, links), incremental re-parse for streaming text.
- [x] `Html` widget for a safe inline/block subset (p, h1–h6, b/i/u/s, a, ul/ol/li,
  blockquote, pre/code, br, hr, img).

## W4 — Anchored overlays

- [x] Anchored placement: side/align, flip on collision, shift into the window, size to
  the available space, arrow offset, re-place on scroll/resize.
- [x] `Select` / `DropDown`: anchored list, type-ahead, arrow/Home/End/PageUp/PageDown,
  scroll to the selected row, `listbox` semantics.
- [x] `ComboBox`: filter as you type, free text or closed set, `combobox` semantics.
- [x] `Menu` / context menu: items, separators, checkable items, shortcuts text,
  submenus with hover intent, keyboard navigation, `on_context_menu` (right click / long
  press).
- [x] `Tooltip`: hover delay, grace period between targets, touch long-press, keyboard
  focus show, `tooltip` semantics.
- [x] `Popover` with arrow (callout).
- [x] Box shadows on nodes (`BuildCx::shadow`), floating layers cast one.
- [x] DSL `shadow` property (drop and inset, `theme.elevation.*` bindable) and its HTML export.
- [x] Framework-drawn `MenuBar` for windows without a native one (Left/Right between
  menus, Alt to focus, mnemonics).

## W5 — Scrolling

- [x] Kinetic scrolling: velocity tracking, platform deceleration curve, flick-catch
  on press.
- [x] Overscroll: bounce (iOS) / glow-stretch (Android) by platform style; rubber-band
  on drag.
- [x] Scrollbars: overlay and classic, draggable thumb, track paging, auto-hide.
- [x] Smooth `scroll_to` / `scroll_into_view` with animation; auto-tail (stick to end).
- [x] Pull-to-refresh.
- [x] Sticky section headers in `List`.
- [x] Scroll snapping and a `Pager` / `Carousel` widget.
- [x] Keyboard scrolling: arrows, Page Up/Down, Home/End, Space; snap-aware.
- [x] 2D virtual grid (`LazyGrid`, fixed and masonry columns).

## W6 — Animation

- [x] Easing set: all Penner curves (quad … bounce, in/out/in-out) and `cubic_bezier`.
- [x] Springs: stiffness/damping/mass, interruption with velocity handoff, settle
  detection.
- [x] Keyframes: multi-stop tracks, repeat count / infinite, reverse, alternate.
- [x] Reduced motion: platform setting honored by transitions, springs and scroll glides.
- [x] Navigation transitions: push/pop slide, fade, platform default; shared-element
  (hero) transition.
- [x] Animated `Toggle` thumb, `Checkbox` check, `Disclosure` chevron.

## W7 — Gestures

- [x] Double/triple tap (`on_double_tap`, `double_click` route).
- [x] Swipe/flick recognizer with direction and velocity.
- [x] Pinch/zoom and rotate recognizers (touch and trackpad magnify/rotate).
- [x] Gesture arena: competing recognizers, claim and reject, nested scroll
  coordination.
- [x] `InteractiveViewer` (pan/zoom container with bounds and inertia).
- [x] Swipe-to-reveal row actions; drag-handle reorder in `List`.
- [x] `Sheet` detents and drag-to-dismiss.

## W8 — Drag and drop

- [x] In-app DnD: drag sources with typed payloads, drop targets with accept/hover/drop,
  drag preview, keyboard alternative.
- [x] OS file/text drop into a window: macOS, Windows, X11 (XDND), Wayland, web, iOS
  (UIDropInteraction), Android.
- [x] Drag out to the OS (files/text) on desktop.

## W9 — Widgets

- [x] `ProgressBar` (determinate/indeterminate), `ProgressRing`, `Spinner`.
- [x] `Stepper` (hold to repeat with acceleration, wrap, keys, spin-button semantics).
- [x] `SegmentedControl` (spring-sliding thumb that follows live segment boxes, fading hairlines, equal shares, roving keys, radio semantics, HTML thumb via CSS vars).
- [x] DSL leaves with no authored size (`Text`, `Button`, a `RadioGroup` option) measure to their content (`fit`) instead of building `Fixed(0)`, on the macro, hot-reload and package paths alike.
- [x] `Chip` (filter/input/assist: selection with a growing tick, a removable cross,
  Delete and an accessibility "Remove" action), `Badge`, `Avatar`, `LinkLabel`.
- [x] `SearchField` with suggestions (built on W4 placement): magnifier, clear button,
  immediate or debounced `on_change`, a pooled anchored suggestion list with the query's
  letters in bold, arrows/Enter/click to take one, Escape closes then clears,
  `searchfield` semantics with expanded + active descendant.
- [x] DSL nodes for the list-backed fields — `ComboBox`, `SearchField` (a typed
  `options`/`suggestions: List<String>`, two-way `value`, `changed`/`submitted`) — and
  their HTML export (`<input type="search">`, `role=combobox`, each over a `<datalist>`):
  the fields' logic moves to shared `viso_ui::combo_box`/`search_field` parts written
  through `PartWrites`, so a view's delivery drives them as a handler does.
- [x] DSL `ColorPicker` node (two-way colour `value`, `alpha`, `wheel`, `changed`) and its
  HTML export (`<input type="color">` plus an opacity range): the picker's parts move to a
  shared `viso_ui::color_picker` the widget and the node both build, the node's parts
  built inside it by the view and reporting through a host sink.
- [x] Slider: vertical, range (two thumbs), step marks, wheel, live thumb; `Knob`
  (rotary). One shared `viso_ui::slider` content and input for the widgets and the DSL
  (`Slider` gains `origin`/`marks`/`axis`/`tint`, new `Knob`); jump-or-pick-up press,
  Shift fine drag, double-click reset, eased thumb, HTML export.
- [x] `ColorPicker` (HSV wheel/square, alpha, hex input): a shared `viso_ui::color_plane`
  content (exact per-vertex mesh square and hue ring, two-tone pucks, swatch over a
  checkerboard), hue/alpha bars on the slider content (`TrackPaint`), hex in four
  forms, hue and saturation kept through grey and black, slider semantics per part.
- [x] `Calendar` (locale first weekday, keyboard grid navigation): `viso_ui::calendar`
  dates (day numbers, month steps held to month ends, ISO weeks), CLDR first weekday and
  hour cycle with `-u-fw-`/`-u-hc-`, a `Days` row content marking selected/today/hover;
  table/row/column-header/cell semantics, roving days, arrows/Home/End/Page Up/Down
  (Shift a year) turning the month, min/max and refused days passed over, week numbers,
  overridable `DateNames`.
- [x] `DatePicker`: a field showing the date that opens a calendar in an anchored
  light-dismiss panel at the picked day (or today, the nearest pickable); a pick closes
  it and refocuses the field, Escape/outside press close unchanged, Tab goes round the
  calendar's buttons and days; `combobox` + `dialog` semantics.
- [x] `TimePicker`: hour/minute (and AM/PM by the locale's hour cycle) spin segments,
  typed digits, minute step.
- [x] `DataGrid`: virtualized rows and columns, resize, reorder, sort, cell/row
  selection, keyboard navigation, `grid` semantics.
- [x] Charts (in `extras/`): line, bar, area, scatter, sparkline.
- [x] Tabs: overflow scroll (a vertical wheel scrolls the strip across, the selected
  tab scrolls into view), close buttons (middle press, Delete; permanent tabs), drag and
  Ctrl+Shift+Page Up/Down reorder; `tabpanel` role; hidden flex children take no fill
  share or gap.
- [x] `Splitter` live relayout: fraction or pinned-pane placement, grab-held drag, pane
  minimums, step/Home/End/Enter-fold keys, `separator` semantics with value and step actions.
- [x] `Skeleton` / shimmer placeholder.
- [x] Form: field validation (required, rules, shown once the focus leaves or on submit),
  error text, `Form` submit (focus the first refused, Enter in a text field), field groups;
  `required`/`invalid`/error-message semantics.

## W10 — Media

- [x] Image decoders: GIF (animated), WebP (lossy/lossless/animated), APNG, BMP, ICO,
  QOI, AVIF (still, grid, sequence; HDR tone-mapped); streaming `Animation` frames;
  EXIF orientation; build-time conversion keeps animated images as written.
- [ ] AVIF on the web through the browser's decoder (`createImageBitmap`).
- [x] Runtime image loading: file path, bytes; decode cache with memory budget (LRU of
  images no node shows), placeholder/error states, `on_load` (DSL `loaded`/`failed`).
- [ ] Image loading from a URL, on the W11 HTTP client.
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
- [x] `open_url` service.
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
- [x] Focus-visible ring drawn by the framework on focused controls (keyboard focus
  only, platform style).
- [x] Access keys on buttons, checks and labelled fields (`&` mnemonics, Alt+letter,
  cues while Alt is held) beyond the menu bar.
