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
- [x] A surface with no sRGB format (a GLES 565 panel, EGL without
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
- [x] AVIF on the web through the browser's decoder (`createImageBitmap`, read back
  through a canvas; a sequence shows its first frame; checked in Chrome).
- [x] Runtime image loading: file path, bytes; decode cache with memory budget (LRU of
  images no node shows), placeholder/error states, `on_load` (DSL `loaded`/`failed`).
- [x] Image loading from a URL: `Asset::url` and the DSL `Image`'s `url`, fetched through
  the app's `Application::image_loader` (any client, e.g. `viso-net`) on the loop's
  thread, then decoded as any image; none installed, it fails.
- [x] Animated image playback in `Image` (frame timing, loop count, pause off-screen / still /
  reduced motion; `animate` in Rust and DSL), decoded off the frame path into the resident texture.
- [x] Video playback (`Video` widget): AVFoundation, Media Foundation, GStreamer,
  MediaCodec, HTML video; the external/YUV texture path in render.
  - [x] `viso-media`: `VideoPlayer` over the OS player (open file/URL/bytes, poll
    ready/ended/failed/waiting, frame when it changes, play/pause/seek/rate/volume/
    mute/loop); AVFoundation on macOS and iOS.
  - [x] Video nodes in `viso-ui` (a resource of their own, provided by the facade;
    per-node playback state and requests), the facade driving players into textures,
    the `Video` widget and the DSL `Video` node (`VideoAsset`, `url`, two-way
    `playing`, `progress`/`ended`/`playing_changed`; HTML export as `<video>`).
  - [x] Video controls: the framework's parts over the picture (play/pause, spinner,
    scrubber, time, mute and volume, full screen; fading bar, pointer, touch and
    keys), shared by `Video::controls` and the DSL `controls`; a leaf's children
    lie over it; node full screen (`set_fullscreen`, the window asked along) and
    the DSL two-way `fullscreen`/`fullscreen_changed`; HTML `controls`.
  - [x] Zero-copy frames on Apple: `ExternalImage` in `viso-handle`, the backend's
    `import_external`/`adopt_external`, IOSurface → Metal (one texture a surface,
    use count held until no frame in flight samples it), the facade adopting
    each picture into the video's texture and copying only where it cannot.
  - [x] Zero-copy on the other backends, with their players:
    - [x] `<video>` → WebGPU: `ExternalImage::WebVideo`, the element's current picture
      copied on the GPU (`copyExternalImageToTexture`, color-managed as the page shows
      it), the canvas readback only where the backend refuses; a player whose pictures
      were refused once is copied without asking again; the example clip tagged BT.601.
    - [x] Shared D3D texture → D3D12: the Windows player draws each frame into one of
      four NT-handle shared textures no backend holds, signals a shared fence, and reads
      rows back only for a paused picture or once a caller asked; the D3D12 backend opens
      it (cached by its holds), waits for the fence on the GPU and copies it into its own
      texture, holding it until the copy completes; `ExternalHolds` in `viso-handle`;
      the decoding device on the high-performance adapter the backend picks.
    - [x] Hardware buffer → Vulkan: the Android reader's buffers lent with per-buffer
      holds and the crop; the Vulkan backend imports each (external format, dedicated
      memory, cached by its holds), samples it through a Y′CbCr conversion made per
      encoding as the device suggests, and draws it into its own sRGB texture's UNORM
      view, owned from the foreign queue for the pass; rows only for a caller that
      copies; a backend-private frozen conversion shader, tested on NV12 planes.
  - [x] Media Foundation player on Windows: the media engine on a thread of its own
    in the multithreaded apartment, decoding through its own D3D11 device, each new
    frame drawn into a BGRA target and read back through a staging texture into
    recycled buffers.
  - [x] GStreamer player on Linux and the BSDs: `playbin` + `appsink` opened at run
    time, on the loop's thread without waiting, each picture handed out mapped in
    place; buffering holds playback, a paused seek shows its preroll, closing on a
    thread of its own; the player tests run on every target with a player.
  - [x] MediaPlayer player on Android: driven through a Java class of `viso-media`'s
    (packed into the APK), the activity handed over once by the facade; pictures through
    an `AImageReader` turned into RGBA, bytes read in place through a direct buffer;
    `Frame::rgba`; a paused video's picture polled for after ready or a seek; the
    `examples/video` app.
  - [x] `<video>` player on the web: a hidden element in the page, each picture the
    browser presents (`requestVideoFrameCallback`) drawn into an offscreen canvas and
    read back as RGBA; bytes through a blob URL; a refused autoplay leaves it paused; a
    changed report wakes the loop at once, so a paused video's controls show.
  - [x] GStreamer pictures as NV12 planes converted on the GPU rather than by
    `videoconvert` on the CPU: `GpuBackend::write_nv12` uploads the planes into
    the texture's own R8/R8G8 images and converts them by matrix, range and
    chroma site in a plain shader (Vulkan); BGRA renegotiated once rows are
    copied; CPU conversion where the backend converts none.
  - [x] Dma-buf import where the decoder exports one: the sink asks for linear NV12
    in `memory:DMABuf` and offers `GstVideoMeta`, so padded buffers come as they are;
    `ExternalImage::DmaBuf` with holds kept on the memory; Vulkan imports the fd as a
    buffer (`VK_EXT_external_memory_dma_buf`) and copies the planes on the GPU into
    the converted texture; a backend that refuses gets mapped planes, then memory at
    hand.
- [x] Camera capture service and `CameraView`.
  - [x] Cameras as live videos in `viso-media`: `VideoSource::Camera(CameraRequest)`
    (which camera, size, rate), `cameras()`, ready on the first picture, paused lets
    the device go, `VideoError::Denied`; GStreamer on Linux and the BSDs (device
    monitor; the source's nearest mode, raw before JPEG, `jpegdec` otherwise).
  - [x] `CameraView` widget and DSL node over the video node (`VideoSource::Camera`,
    `facing`/`device`, front cameras mirrored through the asset's flipped uv,
    two-way `playing`, capturing by default; HTML export through `getUserMedia`),
    the facade opening cameras and listing them (`viso::cameras`).
  - [x] Web: `getUserMedia` into the `<video>` player (ideal size, rate and facing,
    or the exact device; tracks stopped while paused; a refusal is `Denied`;
    `enumerateDevices` listing); the `examples/camera` app.
  - [x] Apple: `AVCaptureSession` pictures as IOSurfaces.
  - [x] Windows: a Media Foundation source reader through the shared textures.
  - [x] Android: Camera2 through the hardware buffer reader, its rotation and the
    permission asked.
  - [x] Cameras as an extension: the protocol stays in `viso-media` (`CameraRequest`,
    `CameraInfo`, `VideoSource::Camera`, `cameras()`, `CameraView`) and a
    `CameraProvider` an extension installs answers it; `Unsupported` without one.
  - [x] Live-picture hooks for extensions, handing pictures the way the platform players
    do: a `LiveSource` (poll, frame, play, pause) a provider opens; Apple
    `Frame::pixel_buffer`, Android `ImageReader`, Windows `SharedCanvas` (the decoding
    device and the shared texture slots), GStreamer `gstreamer_live` over a source
    element, web `MediaStreams` through `web_live`. Without a provider a camera is
    `Unsupported` and none are listed.
  - [x] Android: the platform sets `ndk-context` (the VM, the activity), so `viso-ext`
    crates reach Java; extension Java hears activity results through
    `VisoActivity.Results`.
  - [x] `viso-ext/viso-camera`: the camera backends moved out of `viso-media`
    (AVCaptureSession, Media Foundation, Camera2, the GStreamer device monitor,
    `getUserMedia`) behind `install()`; the camera example moved into it
    (`viso-camera/example`), so the framework's workspace needs no extension.
    - [x] Apple, through `Frame::pixel_buffer`.
    - [x] Android, through `ImageReader` and the extension's own dex.
    - [x] Windows, through `SharedCanvas` / `SharedPictures`, which the media engine's
      player now draws through too.
    - [x] Linux and the BSDs, through `gstreamer_live` over the extension's source element.
    - [x] Web, through `MediaStreams` and `web_live`.
- [ ] Audio as an extension, `viso-ext/viso-audio`; the core keeps only a video's sound
  (the platform player) and the default output the DSL's game audio systems render to
  (`viso_platform::audio::AudioOutput`), whether that goes through a provider too is
  open:
  - [ ] device streams: outputs and inputs listed and watched, a realtime callback
    filling f32 buffers, echo cancellation where the OS has it, the microphone permission:
    - [x] macOS and iOS, CoreAudio I/O units: playback, pause and resume, capture with
      and without echo cancellation (the voice-processing unit at its own rate, its
      output on and silent) run on a MacBook Air and in the iOS simulator;
    - [x] Linux and the BSDs, PulseAudio / PipeWire through libpulse;
    - [x] Android, AAudio, the devices and permission through the crate's Java;
    - [ ] Windows, WASAPI (built and linted for the target, not yet run: no Windows
      machine);
    - [x] Web, Web Audio: one context a stream, blocks moved through an AudioWorklet
      to callbacks on the page's thread, waiting for the first touch (verified in
      headless Chrome with fake devices).
  - [x] playback and recording:
    - [x] sounds in memory played as voices of a mixer on one output stream: volume, pan
      and speed gliding, looping, priority stealing, a limiter on the sum; WAV read by
      the crate;
    - [x] the compressed formats (MP3, AAC, FLAC, Ogg Vorbis) through the platform's
      decoders: Audio Toolbox, GStreamer, the NDK's media codecs, Media Foundation (built,
      not yet run: no Windows machine), `decodeAudioData`; Ogg Vorbis by `lewton` where the
      platform has no decoder for it;
    - [x] music streamed from a file through the same mixer: decoded on a thread of its
      own a block ahead, paused, sought and looped without a gap, its place and length
      read back; seeks land on the frame (MP3 on Android near it, its extractor seeking
      by estimate); AAC's priming and padding trimmed on Android; decoded whole first in
      the browser (a URL plays through `viso-media`'s player, which has no picture to
      show for it);
    - [x] recording what an input captures, to a file or to memory: WAV (16-bit,
      dithered) by the crate, AAC in MPEG-4 through Audio Toolbox, the NDK's encoder and
      muxer, Media Foundation's sink writer (built, not yet run), GStreamer where an AAC
      encoder is installed, and the browser's recorder; encoded on a thread of its own,
      paused without a gap, a level for a meter, and an input that moves to a device of
      another format converted to the file's;
  - [x] the audio session owned by it alone: the app says what its sound is (ambient,
    playback, a prompt, a call) and the crate sets the platform up as the first stream
    opens and gives it up as the last closes — the iOS category and mode, deactivated
    after; Android's audio focus, the streams' usage, and a call's communication mode on
    a headset else the speaker (`MODIFY_AUDIO_SETTINGS` declared with the microphone);
    the PulseAudio role and the WASAPI category. An interruption holds every stream and
    is told to a watch; they play again as it is given back, or as the app asks (verified
    on the Android emulator through a ringing call; iOS built and linted, not yet run).

## W11 — Networking

The core does UI only: networking is the app's business. The core keeps the hooks —
tasks that await any future (`cx.spawn_then`), `native task` for the DSL, and the
package's network permission — and `viso-ext/viso-net` wraps each platform's own stack
behind one easy client, as Flutter's dio does, compiled for the target alone.

- [x] The package's `network` permission: Android `INTERNET` and `ACCESS_NETWORK_STATE`
  in release builds too, the MSIX `internetClient` capability; Apple apps need none.
- [ ] `viso-ext/viso-net`, an HTTP client and WebSocket on each platform's own stack
  (its certificates, proxies, VPN and cookies), whose futures need no runtime:
  - [x] the client: a base URL, headers, query, timeouts, redirects, interceptors on
    the request, the response and the error (auth, retry, logging), a cancel token and
    a dropped future cancelling, bodies of bytes, text, JSON (`serde`, a feature) and
    ASUN (`application/asun`, `application/asun+binary`, on by default), a client-wide
    codec chosen by the response's type, a mock backend for tests;
  - [x] Apple, URLSession with a delegate: the body streamed as it comes, held back by
    the delegate waiting while the app reads slower (a suspended task reads on), so a
    session runs one request at a time and is kept for the next to its origin; an
    untyped body's first 512 bytes held back, as URLSession sniffs its type;
  - [x] Android, `HttpURLConnection` through the crate's own dex, a thread per request
    held while the app reads slower; a dev build's manifest allows plain HTTP, for a
    server on the developer's machine;
  - [x] Linux and the BSDs, libcurl opened at run time: one thread on curl's multi
    interface, a held body's transfer paused, cookies, DNS and TLS sessions shared;
    redirects followed alike on every platform, credentials kept from another origin;
  - [x] Windows, WinHTTP, asynchronous: the next read asked for only while the app
    keeps up, HTTP/2, gzip, the system's proxy (built and linted, not yet run on
    Windows);
  - [x] Web, `fetch` with the body read from its stream only while the app keeps up,
    the page's clock and one `setTimeout` for the deadlines; the browser follows
    redirects itself, and tells an upload's progress only at its end;
  - [x] uploads and downloads: a body of parts, bytes and files read only as they go
    (sent again from any point where a redirect asks), multipart forms, a body saved
    to a file on a thread of its own and moved in once whole, progress both ways;
    the web sends no file;
  - [x] WebSocket: text, binary, ping and close, opened through the client's base URL,
    headers and interceptors, the reading held back while the app reads slower;
    URLSessionWebSocketTask, libcurl's (opened on a thread, then run by the worker),
    WinHTTP's (built and linted, not run), the browser's (`WebSocketStream` where there
    is one, held back; else `WebSocket`), and the crate's own protocol over Java's
    `SSLSocket` (the system's TLS and certificates) on Android;
  - [x] native packages: an extension crate's native libraries reach `.vs` code:
    - [x] native package format: a library's schema written to and read from JSON,
      lossless, read schemas interned as stubs; ADR 0051;
    - [x] discovery: the app's direct dependencies declaring
      `[package.metadata.viso.natives]` (schema, Rust static, feature), through
      `cargo metadata`, cached for the process;
    - [x] `view!`/`component!` resolve a path no standard library has against the
      packages and hand the libraries the module calls to its host, which links them
      (one shared registry per set), as do its game, a loaded release package and the
      dev session's patches;
    - [x] `viso check`/`export`/`dev`/`schema` and the LSP compile against them (the
      LSP now also reports an import naming nothing);
    - [x] `Viso.toml` a compile dependency of the forms, so a changed grant rebuilds
      them;
  - [x] `.vs` reaches the network through the app's own domain natives, not raw HTTP:
    - [x] `viso-net` carries no DSL library (its `net` library goes); the spec's
      examples call domain natives (`Api::profile(id)`), the Rust behind them using
      `viso-net`;
    - [x] `viso::native`: the public surface for writing native libraries (types,
      `native!`, handles) and checking a package's schema in its tests, so an app's
      natives crate depends on `viso` alone;
    - [x] `Result<T, E>` in native schemas: a native returns one and `.vs` matches
      it;
    - [x] data records in native schemas: `#[derive(NativeData)]` on a Rust struct is
      a native value type whose fields `.vs` reads, carried in the JSON schema;
    - [x] an app whose natives crate (a sibling in its workspace) answers
      `profile(id) -> Result<Profile, String>` through `viso-net`, called from `.vs`;
      docs (DSL §47, ADR 0051, Architecture).
  - [x] `a ?? b`: parsed but neither typed nor lowered; `Option<T> ?? T` is `T`, `b` evaluated only on `None`;
  - [ ] ASUN rows decoded as they arrive, through an incremental decoder in `asun`
    (deferred: not wired in for now; picked up once `asun-rs`'s pending work lands).
- [x] `open_url` service.
- [x] Network reachability: `viso-ext/viso-connectivity`, the app's own choice (checked
  and watched on macOS).
- [x] `viso-services` overlaps `viso-ext` (share, notifications, launcher, file dialogs,
  secure storage, haptics, permissions): the core keeps haptics and the launcher (both
  serve controls and links) plus `app_data_dir` / `LocalStorage`; file dialogs, share,
  notifications and secure storage are `viso-file-picker`, `viso-share`,
  `viso-local-notification` and `viso-secure-storage`, each with its own permission.

## W12 — Platform extras

- [x] Permissions: camera, microphone, location, photos: each plugin's own `permission()` /
  `request_permission() -> Reply<PermissionState>` (`viso-common`'s `Granted | Denied |
  Prompt`): `viso-camera`, `viso-audio`, `viso-geolocator`, `viso-local-notification`;
  Android tells denied from never asked. Photos come through the system pickers, which
  need none (`viso-file-picker`). Run on macOS, Android and the web; built for iOS, Windows
  and Linux.
- [x] Location service: `viso-ext/viso-geolocator`: a one-off and the last known position,
  a watch with accuracy and distance filter that ends on denial and reports the service
  turned off, the settings pages, and a mock; run on macOS, Android and the web.
- [x] Gamepad backend (GameController, XInput/GameInput, evdev, web Gamepad API) into the
  game input.
  - [x] one standard gamepad model in `viso-platform`, polled once per frame head (events
    only on change, no steady-state allocation), into the game input: every pad merged
    into local player 0, `env.input.gamepad_available` following connections, a gone pad
    releasing what it held; headless pads for tests (platform, runtime, facade and
    `.vs` game tests).
  - [x] macOS / iOS: GameController.framework, loaded at the first poll; elements cached at
    connection, the list re-read only on its connect/disconnect notifications; rumble
    through the controller's CoreHaptics engine (macOS: framework, notifications and the
    element mapping verified with `controllerWithExtendedGamepad`, and in a running app
    with no pad attached; iOS: clippy only; rumble unverified without a pad).
  - [x] Windows: XInput loaded at the first poll, connected slots read once a poll (skipped
    while the packet number stands), empty slots probed one per 250 ms, the guide button
    through `XInputGetStateEx`, rumble stopped at its deadline with the pump woken for it
    (clippy for `x86_64-pc-windows-msvc` and the button/axis mapping unit-tested; not run
    on Windows). GameInput is not used: it ships only with the GDK redistributable or
    recent Windows 11 and has no stable ABI outside the GDK headers.
  - [x] Linux: evdev read directly (no udev), pads by `BTN_SOUTH`, hotplug through inotify
    on `/dev/input`, `SYN_DROPPED` resync, xpad's X/Y swapped to position, `FF_RUMBLE`
    effects; KMS input leaves pads alone (verified in podman on a real kernel: a
    uinput-made pad hotplugged, read, rumbled through an uploaded effect and unplugged;
    decoder unit-tested over recorded events).
  - [x] Android: an `InputManager` device listener started at the first poll, the view's
    gamepad keys and joystick motion routed to the loop thread (woken at the first change
    since a poll), rumble through the device's `VibratorManager` / `Vibrator` (verified on
    the API 36 emulator with a `uinput`-made pad: connection, buttons, stick, trigger, hat
    and departure read in a running app, and its rumble reaching the device as `EV_FF`;
    the key/axis mapping unit-tested).
  - [x] Web: the Gamepad API, `gamepadconnected`/`gamepaddisconnected` waking the loop,
    `getGamepads()` called once a frame only while a pad is connected, a pad read only when
    its `timestamp` moved, rumble as a `dual-rumble` effect (verified in headless Chrome
    over CDP with a fake `navigator.getGamepads` in a running game: connection, buttons,
    stick, trigger and departure, a frame-rate poll only while connected, the unchanged
    timestamp skipped, the rumble effect played, and `env.input.gamepad_available`
    switching the view in the accessibility DOM; the mapping unit-tested).
  - [x] Rumble from game systems: `kit.rumble(strong, weak, seconds)`, a Presentation command
    the stage keeps (the last one a frame asks for), handed by the game's frame client to
    the facade and played on every pad connected while its window holds input focus
    (the DSL kit test and the facade's headless gamepad tests: one pad and two pads
    rumbled by a jump).
- [x] Rich clipboard: images and HTML. `ClipboardData { text, html, image }`, pastes name
  the `ClipboardKinds` they take and get the richest; `set_clipboard`, tokened
  `request_paste`, `PasteRequested` handshake for gestures; paste targets and
  `EventCx::pasted` in the UI, text fields keep plain text.
  - Verified: headless facade test (copy, gesture, asked paste, text field); macOS run
    (copy read back by `osascript`/JXA as `public.png` 256×128 + HTML + text, the promised
    PNG settled on exit; browser HTML and a TIFF-only image pasted); headless Chrome over
    CDP (copy read back as `ClipboardItem` text/html/png with the right pixels, asked
    paste and `paste` gesture of a foreign item); X11 in podman with `xclip` (targets,
    HTML, text and PNG out; HTML, text and PNG in; a 3 MB PNG both ways over INCR).
  - Not run: Windows, iOS and Android (clippy only; the Android Java shell compiles with
    `javac`), Wayland under a compositor (unit tests only: no seat in headless weston).
- [x] Tray / status-bar icon with menu.
  - `viso::shell::set_tray` / `tray_supported` / `icon`, `Application::on_tray`; picks go
    through `on_menu_command`; `PlatformCaps::tray`; headless records it (facade tests).
  - macOS (`NSStatusItem`): verified with the `shell` example through Accessibility: the item
    and tooltip show, a click reaches `on_tray`, the click-opened menu lists its entries, a pick
    reaches `on_menu_command`, removal takes the item away. Secondary-click menu not exercised.
  - Linux (StatusNotifierItem + dbusmenu on a background zbus thread): verified over a real
    session bus in a container against a stand-in watcher (registration, icon pixmaps, tooltip,
    layout, a menu pick, Activate, update, removal); no real panel tried.
  - Windows (`Shell_NotifyIconW`, re-added on `TaskbarCreated`): compile-checked only.
- [x] Deep links / URL schemes.
  - `[package.links]` schemes and hosts → Info.plist `CFBundleURLTypes`, iOS associated
    domains, Android intent filters (`autoVerify` for hosts), MSIX `windows.protocol`,
    `.desktop` `x-scheme-handler`; dev builds too; generated output unit-tested.
  - `RawEvent::OpenUrl`, after launch, cold and warm: macOS `application:openURLs:`, iOS
    scene URL contexts and universal-link activities, Android VIEW intent + `onNewIntent`,
    Windows argv + `WM_COPYDATA` hand-over to the running instance (HKCU registration when
    unpackaged), Linux argv + runtime-dir socket hand-over, web fragment + `hashchange`.
  - `Application::on_open_url(cx: &mut EventCx, url)` with the launch window's root context;
    links arriving before the window is built wait for it; `examples/links`. The DSL has
    no app lifecycle, so it takes no part.
  - Ran: macOS packaged app cold and warm through LaunchServices; Android emulator cold,
    warm and from the background; Linux X11 (Xvfb) cold plus two hand-overs. Not run:
    iOS, Windows, web, Wayland, universal/app links.
- [x] Custom cursors and pointer lock.
  - [x] Image cursors: `CustomCursor::new(CursorImage)` registers straight RGBA, a hotspot
    and a scale once and names it `CursorIcon::Custom(id)`; `viso::cursor::decode` reads an
    encoded image. Each backend builds its OS cursor on first show and caches it (macOS
    NSCursor, Windows CreateIconIndirect per DPI, Wayland shm surface, X11 Render cursor, KMS
    hardware cursor, web CSS `url()`/`image-set`, Android `PointerIcon`); iOS reports
    `custom_cursors: false`. The cursor follows the mouse right after input
    (`FrameDriver::after_input`), not at the next frame. Verified: macOS live (the
    `pointer` example's 64 px @2x ring reads back from the window server as a 32 pt cursor,
    hotspot 16,16; the system crosshair over the other panel), headless facade tests, unit
    tests (registry, resampling, cache, KMS drawing); web in headless Chrome over CDP (the
    canvas's computed cursor is the `image-set` of the PNG data URL over the image panel,
    `crosshair` over the other); Windows, Linux, Android, iOS compile and pass clippy only.
  - [x] Pointer lock with relative motion: `EventCx::lock_pointer(id)` /
    `unlock_pointer`, `PlatformApp::set_pointer_lock`, `PlatformCaps::pointer_lock`,
    `RawEvent::PointerMotion` (logical-point deltas) and `PointerLockChanged`; the locked
    node gets every mouse sample, its motion through `EventCx::pointer_motion` and each
    change through `EventCx::pointer_lock_change`; Escape, focus loss and the node's freeing
    end it; a host without it answers the ask with the lock ended.
    - [x] Core, UI routing, facade, headless host. Verified: headless facade tests (lock on
      press, motion deltas, presses routed to the locked node, Escape, focus loss, refusal
      without the capability), unit tests (lock state, motion coalescing).
    - [x] macOS: freeze and hide the cursor at the view's centre, `deltaX/deltaY`, release
      on focus loss and window close. Verified live with the `pointer` example: a press
      locks (logged `pointer lock true`), ten posted relative moves of (5, 3) arrive as ten
      (5, 3) motions, Escape releases (`pointer lock false`). The first hardware delta after
      the warp to the centre carried the warp's distance (-268 for a -260 warp), so the lock
      now takes the warp back out of the next delta; that correction was not re-run live.
    - [x] Web: `requestPointerLock`, `pointerlockchange`/`pointerlockerror`, `movementX/Y`.
      Verified in headless Chrome over CDP: a click locks the canvas, two pointer moves
      arrive as motions (5, 3) and (-2, 7), `exitPointerLock` reports the lock ended.
    - [x] X11: `XIGrabDevice` with no cursor, raw `XI_RawMotion` deltas, the pointer pinned
      back to the window's middle, release on focus loss. Verified under Xvfb in podman with a
      platform-level test app driven by xdotool: a press locks (`PointerLockChanged` true),
      relative moves (5, 3) ×3 and (-7, 0) arrive as exactly those motions while the pointer
      stays at its pin and no moves are reported, Escape releases, a later lock ends on focus
      loss.
    - [x] Windows (`ClipCursor` + `WM_INPUT` raw input), Wayland (pointer constraints +
      relative pointer), KMS (libinput relative motion), Android (`requestPointerCapture`,
      API 26), iPadOS (`prefersPointerLocked` + `GCMouse` motion): compile and pass clippy
      (Java against android-36), not run.
    - Game input has no mouse delta to feed (its input schema covers keys, pad buttons,
      sticks and touch buttons only).
- [x] Fullscreen change events on every OS (Android hides the system bars, iOS the status
  bar and home indicator).
- [x] Window state persistence.
  - `WindowConfig::persist` (default `Some("main")`, `None` opts out) keeps size, position and
    maximized/fullscreen under `window/<name>` in the facade store; stored after the frame
    rests 400 ms and at close / background / exit; restored at creation, off-screen frames
    moved and shrunk onto a screen. Platform: `RawEvent::Moved`, `Window::mode`,
    `WindowConfig::maximized`. Facade tests on the headless backend (move/resize, fullscreen,
    opt-out).
  - macOS: verified by relaunching the `shell` example after Accessibility-driven changes: a
    moved and resized frame, fullscreen, and zoom all reopened as left (unzoom and leaving
    fullscreen return to the stored frame); a stored frame far off-screen opened on the
    screen. Fixed the macOS loop dropping the control flow (and timer deadlines) returned
    while it delivered queued events.
  - Windows (`WM_MOVE`, `IsZoomed`, `rcWork`), X11 (`ConfigureNotify`, `_NET_WM_STATE`,
    RandR monitors) and Wayland (size and maximize only; the compositor places): compile-
    and clippy-checked only.
- [x] Dock/taskbar badge.
  - `viso::shell::set_badge(Option<AppBadge>)` (`Count` or `Label`), `badge_supported`;
    `PlatformCaps::badge`; headless records it (facade tests).
  - macOS (`NSDockTile` badge label): verified with the `shell` example through the Dock's
    Accessibility tree: picks from the tray menu showed 1, then 2, and clearing removed it.
  - Web (`navigator.setAppBadge` / `clearAppBadge`): verified in headless Chrome over CDP; the
    example's buttons made the calls set(1), set(2), set() for a label, clear().
  - Linux (Unity LauncherEntry `Update` signal and `Query`): verified over a real session bus
    in a container; no real dock tried.
  - Windows (overlay icon drawn with GDI, re-set on `TaskbarButtonCreated`) and iOS
    (`setBadgeCount`, falling back to `applicationIconBadgeNumber`; needs notification
    authorization with badges): compile-checked only. Android: unsupported.

## W13 — Effects widgets

- [x] `BlurView` / glass panel over the backdrop blur and material built-ins: a
  `backdrop: Option<Backdrop>` (blur, saturation, tint, grain) on every node draws the
  renderer's frosted material under the box's fill, outside its own fade; `BlurView` is a
  column showing the standard glass, `blur_view` in Rust; the web export writes a
  `backdrop-filter` under a flat tint (seen in a headless snapshot over coloured rows).
- [x] Ink/ripple press feedback as a standard primitive; haptics wired into controls: a
  `ripple: Bool` on every node (on for buttons and chips on Android) spreads ink from the
  press over the box, clipped to its corners, fading once let go; a finger's change to a
  toggle, chip, stepper, select, segmented control or stepped slider plays the
  platform haptic, in the DSL controls and the Rust widgets alike. Not yet seen in a
  pixel snapshot: the scenario language has no held press.

## W14 — 3D extended (after W1)

- [x] glTF 2.0 loader (meshes, materials, textures, node hierarchy, animation) as an
  asset. — Own bounds-checked reader in `viso_render::scene3d::model` (`.glb`/`.gltf`,
  data/relative URIs, sparse/quantized accessors, generated flat normals and tangents,
  step/linear/cubic animation, pose solve, joint matrices); `AssetKind::Model` decoded on
  the asset worker and made resident per window (`NodeStore::scene_model`,
  `SceneFrame::push_model`). `COLOR_0` multiplies the base color and `TEXCOORD_1` feeds
  any map naming `texCoord: 1`, through a 16 B `VertexExtra` only meshes that need it
  carry (`create_scene_mesh_extra`, the `Extra*` programs; plain meshes stay 48 B).
  Verified: unit tests incl. hostile inputs, the six Khronos samples decode, a facade
  residency test, models drawn on the CPU raster; VertexColorTest, BoxVertexColors,
  MultiUVTest and synthetic color / second-set squares (skinned too) drawn alike on the CPU
  raster and Metal.
- [x] PBR metallic-roughness material with image-based ambient. — `Material` /
  `create_scene_material` / `SceneFrame::push_with`; glTF materials with all five maps
  (mip chains built on the asset worker, `create_texture_mipped`); GGX + Smith + Schlick
  with specular AA, `Environment` (prefiltered equirect atlas + SH9, from a radiance map
  or the sky/ground gradient), PBR Neutral tonemap; blend sorted back to front. The CPU
  raster runs the same expression (`viso_gpu::scene::shade_mesh`). Verified: CPU + Metal
  drawn and compared, WGSL through naga, GLSL ES / HLSL through glslang, Khronos samples.
- [x] Shadow maps (cascaded for the directional light). — `Lighting::shadows: Shadows`;
  three cascades fitted to the receivers' depth range (slice spheres, texel-snapped, depth
  reaching every caster), a per-slot `3r × r` RGBA8 atlas of 24-bit packed depth drawn
  by `SceneProgram::Depth` (no sampleable depth or float target needed), normal offset +
  slope bias, 4×4 tent PCF with cascade cross-fade. Verified: CPU + Metal + translated
  WGSL draw the same shadowed scene, GLSL ES / HLSL compile, lane test, Khronos samples.
- [x] Skinned meshes. — `SkinnedVertex` (4 joints / 4 weights as packed u16),
  `Renderer::create_scene_skinned_mesh` + `SceneFrame::push_skinned(mesh, material,
  instance, &joints, bounds)`, palette of up to 256 joints in the third uniform block,
  `SceneProgram::SkinnedMesh` / `SkinnedDepth` skin on the GPU (casting skinned shadows
  too); `push_model` poses glTF skins, `SceneModel::bounds(&pose)`. A palette whose joints
  mirror culls front faces for the draw; joints mirroring some triangles and not others
  draw unculled while the skinned programs discard back-facing fragments. Verified: CPU +
  Metal + translated WGSL draw the same skinned scene, GLSL ES / HLSL compile, lane tests
  (mirrored and mixed palettes, CPU and Metal), CesiumMan / Fox.
- [x] SSAO and sky. — `SceneFrame::sky: Option<Sky>` draws a Preetham daylight sky (with
  night / twilight fade and sun disc) behind opaque meshes, `Environment::from_sky` lights the
  frame by it, `Lighting::sunlit(&sky)` gives the sun's direct light. `Lighting::occlusion:
  AmbientOcclusion` adds half-res SSAO (view depth, 12-tap hemisphere, two bilateral blur
  passes) that darkens only the environment's light. Verified: CPU + Metal + translated WGSL
  draw the same sky and occlusion, GLSL ES / HLSL compile, lane tests, Khronos samples.

## W15 — Code editor widget (in `extras/`)

- [x] `CodeEditor`:
  - [x] rope document, multi-cursor, undo: a B-tree rope with line counts (random edits
    checked against a `String`), edits as change sets of one change a cursor with line
    splices that every line array follows, sorted merged cursors keeping a column goal, and
    a bounded history that folds a typing run at every cursor into one change each, broken
    a word at a time; the widget draws only the lines on screen, as rows of a virtual list
    keyed by line ids; headless facade tests type, compose, delete, break and indent lines,
    click, drag, double-click, add cursors with Alt-click and Alt-Cmd-Down, cut once per
    shortcut and paste a whole line, then undo and redo it all;
  - [x] syntax highlighting through the `.vs` lexer and a TextMate-free token API: the
    tokenizer is its own leaf crate, `viso-lexer` (the compiler re-exports it at the same
    paths), which `.vs` highlighting runs a line at a time, carrying across lines only an
    open block comment's depth or a raw string's hashes; a `Language` is a line lexer over
    `u32` states, and the editor keeps each line's end state, re-lexing from an edited line
    only until the states agree again and past the screen only when asked; tests check the
    line-at-a-time classes against the whole-file tokenizer over the prelude and samples,
    and through the facade a typed `/*` recolors the lines below and `*/` restores them;
  - [x] line numbers, folding, wrapping, inlays and decorations, search/replace: each row
    carries its piece of the gutter (number, fold chevron, worst diagnostic), moved by the
    list's sideways scroll in the pass that scrolls, so it stays put with no frame of lag;
    folds by indentation hide lines from the list (rows map through hidden counts), wrapped
    lines are listed with exact row extents, inlays and a fold's placeholder take cells on
    the grid, decorations draw backgrounds, lines and squiggles in named sets that follow
    edits, and a find bar searches as it is typed (literal across chunks, or a regex a line
    at a time; only edited lines are searched again), steps, replaces one or all as one undo
    step, and matches the bracket at the caret outside strings and comments; headless
    facade tests check the numbers and the pinned gutter under a scroll, a fold clicked
    from its chevron and stepped over, a wrapped line moved through a row at a time, an
    inlay shifting the text and a diagnostic's squiggle and gutter mark, and the bar's
    find, step, retype, replace and replace-all, and Escape back to the text;
  - [x] LSP client hookup: a `LanguageService` trait the widget asks for diagnostics,
    completions, hovers, inlay hints and definitions, each answer a future over a snapshot
    of the text that the edits since are mapped through; the `lsp` feature (desktop only)
    adds an `LspClient` speaking JSON-RPC to a server over its pipes, negotiating UTF-8
    columns, sending full or incremental changes from a writer thread and answering the
    server's own requests; the widget shows a fuzzy-filtered completion list, squiggles,
    a hover box on a resting pointer and jumps to definitions; the session keeps the
    service, its edit log and its diagnostics wait, so a view built again into the same
    slot shows the same session with what the server reported, a report on its way
    included; tests drive a fake service
    through the facade, `viso-lsp` over in-process pipes (diagnostics, hover, definition,
    an edit clearing an error, and squiggles in the widget kept across rebuilds on one
    shared client) and rust-analyzer when
    installed (UTF-8, hover, inlay hint, completions and a definition after incremental
    edits); `examples/code-editor` edits a `.vs` file with `viso-lsp`, checked in a
    headless run's snapshots (highlighting, gutter, find matches, a caret and a squiggle
    from the server); on 100k lines in release a facade run opens in about 6 ms, takes
    about 1 ms a keystroke to its frame and 13 MiB resident.

## W16 — Missing in both

- [x] Localization: ICU-style message formatting (plural/select), number/date formats:
  `{d, date}` (`short` to `full`) and `{t, time}` (`short`, `medium`) write the prelude's
  `Date`, `Time` and `DateTime` through CLDR's Gregorian patterns in the reader's locale
  and hour cycle (the `dates` feature, under 500 KB; ISO 8601 without it), `{n, number,
  integer}` rounds half to even; the web export writes them through `Intl`. Checked in
  en-US, de-DE and ja-JP under the dev runtime, the release package and Node.
- [x] Printing (desktop) and PDF export of a view: `viso::pdf::export(Pdf::new(view))`
  lays a view out at the page width, cuts it between lines and around leaves (with
  `page_break` and `keep_together`), adds a header and footer per page, and writes PDF
  1.7 through `viso-pdf`: boxes, paths, gradients, meshes, clips and blended groups as
  vectors, text as Type 3 outline fonts that select and copy, images, shadows, shaders
  and scenes as 300 dpi rasters (left out on web and GLES). The system print dialog is
  the `viso-print` plugin in viso-ext (PDFKit, UIPrintInteractionController,
  PrintManager, Direct2D print control, the XDG portal, the browser). Checked in PDFKit:
  pages, footers, text extraction and the shadow raster.
- [x] Biometric authentication service: `viso-ext/viso-auth`: LocalAuthentication,
  BiometricPrompt, Windows Hello, fprintd and WebAuthn behind `availability()` and
  `authenticate(reason) -> Reply<()>`, with a mock; run on macOS, the Android emulator
  and Chrome. `USE_BIOMETRIC` comes from the plugin's `android/manifest.xml` fragment,
  Face ID's reason from `[package.permissions] biometrics`.
- [x] Push notification registration (APNs/FCM): `viso-ext/viso-push`: APNs through a
  delegate hook added at run time, FCM through Google Play services' IPC (no SDK), Web
  Push with its service worker half; the token as `Reply<String>`, a `Messages` stream,
  and a mock. Run on Android, the web and the iOS simulator, which answers a device
  token now that its `aps-environment` is linked into the app; an FCM token awaits a
  real Firebase project, a device APNs one real signing; Windows and Linux are
  Unsupported. The receiver, permissions, `remote-notification` mode, entitlement and
  worker come from the plugin's fragments (checked with `aapt dump xmltree`, `plutil`
  and the simulator).
- [x] Background tasks service: `viso-ext/viso-background`: BGTaskScheduler,
  JobScheduler, NSBackgroundActivityScheduler and Background Sync, `tasks!` registering
  before `main`, `Request { after, every, network, charging, processing }`, and a mock.
  Run on Android (a cold start too), macOS, the web and in iOS registration; Windows and
  Linux are Unsupported. The job service, permissions, background modes and worker come
  from the plugin's fragments, the task ids from the app's own `ios/Info.plist`; an app
  linking it beside viso-push ran its job on the emulator from the merged manifest.
- [x] Focus-visible ring drawn by the framework on focused controls (keyboard focus
  only, platform style).
- [x] Access keys on buttons, checks and labelled fields (`&` mnemonics, Alt+letter,
  cues while Alt is held) beyond the menu bar.

## W17 — Maps (`extras/map`, data and navigation in viso-ext)

A vector map view an app embeds like any control, offline-first, drawn through the
retained renderer: tiles are nodes whose meshes are built once off the main thread, and
panning, zooming between levels and rotating only move transform roots, so a steady pan
uploads nothing. Format readers, network sources and routing are not UI and live in
viso-ext.

- [x] Placement and seams: `extras/map` (`viso-map`) on ui, widgets, render, text and
  math (check-deps allowlist, Architecture §10); a general retained-drawing node content
  in `viso-ui` (primitives in node-local space, no map knowledge); a `TileSource` trait
  (`request(tile) -> Reply<Option<TileData>>`, cancel) so any reader plugs in.
  Built `extras/map` on ui, widgets, render, text, math and services (allowlist,
  Architecture §8/§10); `Content::Drawing` (`viso_ui::Drawing`: shared primitives in a
  drawing space mapped onto the box, compared by identity, PAINT only); `viso_ui::work`,
  a resident worker pool the facade sizes from the footprint and drains inline where no
  thread runs; `TileSource` (`request -> Reply<Option<Vec<u8>>>`, `cancel`) with every
  `Fn(TileId) -> Reply<..>` a source and a `MemorySource`. Unit tests in ui and map.
- [x] Geo math and camera: Web Mercator, lon/lat ↔ world ↔ view, fractional zoom,
  bearing, bounds and padding, `fit_bounds`, min/max zoom and a bounded region,
  `fly_to` (smooth zoom-out-pan-in) and `ease_to`, camera state as typed props and
  events (`.vs` bindable).
  `viso_map::camera` (Web Mercator, `View` project/unproject with padding, `Bounds`,
  `Limits` with min/max zoom and a bounded region) and `MapHandle::{jump_to, ease_to,
  fly_to, fit_bounds, pan_by, zoom_by, rotate_to}`; `CameraCells` are four typed `Float`
  cells the view writes and follows (bindable), `on_viewport` fires when the camera
  rests. Unit tests in camera and headless tapes in `tests/view.rs`.
- [x] Gestures and input: drag pan with fling, pinch zoom about the focal point,
  two-finger rotate (snapping to north), wheel/trackpad zoom and pan, double-tap zoom
  in, two-finger tap zoom out, keyboard (arrows, +/-, rotate), focus and semantics
  (role, label, center and zoom as value, keyboard equivalents for every gesture).
  Drag with fling (Alt-drag turns), touch pinch about the fingers' middle with a turn
  threshold and a north snap, trackpad scroll pans and its pinch zooms, wheel notches
  zoom about the pointer (`viso_ui::route_wheel`, a wheel taker seam), double tap in, two
  finger tap out, arrows/Shift-arrows/`+`/`-`/`N` on the focused map, `Role::Image` with a
  label, a hint naming the keys and "lat, lon, zoom" as the value. `drive_scripted` tapes
  in `tests/view.rs`.
- [x] Tile pyramid: the covering set for the rotated view, parent fallback while
  children load, overzoom past the source's max zoom, prefetch ring, stale requests
  cancelled, a byte-bounded LRU cache, cross-fade on arrival, one node per tile.
  `pyramid.rs`: the covering set for the rotated, padded view (nearest the focus first),
  drawn ancestors while children load, overzoom past the source's max, a prefetch ring, a
  request budget, stale requests cancelled (flag, task and source), a byte-bounded LRU,
  one drawing node per tile under a camera node. Cross-fade on arrival is not done: an
  opacity layer inside a turned transform root draws nothing in the renderer today.
- [x] Vector tiles: MVT decode (protobuf, geometry commands, properties, gzip),
  features filtered by the style before tessellation, on worker threads.
  Decoded and built per tile on `viso_ui::work` (`load.rs`); a job whose tile was dropped
  before it started does nothing; rebuilds start from the decoded tile.
- [x] Tessellation: polygons with holes (AA fringe, clipped to the tile square), lines
  with joins, caps and dashes held at a constant screen width across fractional zoom,
  points; meshes per style layer in tile-local units; no main-thread geometry.
  `tess` + `build.rs`: one mesh per tile, extruded on the GPU by screen points so line
  widths hold across fractional zoom; built only on workers.
- [x] Style: a typed style (background, fill, line, symbol, circle, raster, hillshade)
  with filters, zoom stops and data-driven values; light and dark built-in styles for
  the OpenMapTiles and Shortbread schemas; theme switching without refetching.
  `style` (typed props with zoom stops and data values, filters) and `builtin`
  (OpenMapTiles and Shortbread, light and dark). `MapHandle::set_style` rebuilds the shown
  tiles from what they decoded without fetching again (tested).
- [x] Labels and symbols: point and line-following labels shaped by `viso-text`
  (scripts, CJK, `name:xx` fallback to the reader's locale), collision with priority,
  kept upright under rotation, fade in and out, SVG icons and road shields, a label
  pass per camera change bounded in time.
  `label.rs` picks each symbol layer's text on the worker (`name:xx` by the reader's
  `languages`, each tag then its primary subtag, then `name`; the chosen language sets the
  text's locale) and where it sits: points, polygon centroids, the middle of a line, or
  along a line every `spacing` where it runs straight. `view/labels.rs` places them when the
  camera rests, later layers and lower ranks first, against a grid of the boxes taken and
  within 4 ms, then moves them with the camera; line labels turn with the map and are kept
  upright, labels fade in and out (at once under reduced motion). Text is `viso-text` runs
  over four offset halo copies; `IconSet` takes SVG icons (tinted or not) and a built-in dot;
  road numbers sit on shields. Line labels bend with their line glyph by glyph: the
  worker cuts the text into clusters (marks, joiners and joined scripts kept whole) and
  keeps the line's stretch around each anchor; the view measures each cluster's advance
  once, sets the glyphs along the line as it shows at any turn and tilt, reading upright,
  leaves a run out where the line turns more than 45° between neighbouring glyphs, and
  takes each glyph's turned box against the others; tilted, labels and their stretches
  sit on the relief under them (`tests/labels.rs`: arcs at six turns and tilts, repeats
  along a long line, real Luxembourg streets). Headless pictures (Latin, Greek, Cyrillic, Arabic, Japanese, Chinese; turned;
  icon) and unit tests.
- [x] Raster and terrain: raster XYZ layers (PNG, JPEG, WebP), hillshade from DEM
  tiles (Terrarium and Mapbox RGB encodings).
  Each raster or hillshade style layer gets its own pyramid over a named
  `RasterSource` (256 px tiles draw a level deeper); bytes are decoded, and DEMs shaded
  (Horn gradient, slope by aspect against the light), on `viso_ui::work`, handed to the
  asset table once and shown by leaves placed like vector tiles. A tile's vector layers
  split at the first image layer into a part under and a part over the images.
  Headless test: a half-opaque checker under the roads and a shaded ridge, with zero
  upload while dragging. Open: DEM tiles shade without their neighbours' edge pixels,
  so a faint seam can show at tile borders.
- [x] Overlays: markers that are ordinary components anchored at lon/lat, polylines
  (route casing, traveled part dimmed), polygons, circles in meters, a GeoJSON source
  styled like tile data, clustering, the user's position puck with heading and
  accuracy.
  `overlay.rs` builds `Polyline` (casing, traveled fraction in its own colors),
  `Polygon` and `Circle` (meters) once into a mesh in a world frame placed like a tile,
  so pans and zooms only move them; `Marker` is the app's component in a sizeless
  wrapper moved to its place by its anchor; `Cluster` gathers markers per zoom level and
  opens to fit them when tapped; `Puck` is a dot and heading wedge turned with the map
  over an accuracy circle. GeoJSON (`GeoJson::parse`, or features in code) becomes a
  layer of each tile it crosses, clipped on the worker, drawn by the style's layers and
  drawn where the source has no tile. Headless pictures and pixel checks in
  `tests/view.rs`; unit tests in `overlay` and `geojson`.
- [x] Queries and events: tap and long-press at lon/lat, rendered features at a point,
  viewport changed, marker tapped.
  `on_tap`/`on_long_press` give a `MapPoint` (lon/lat and view point), `on_viewport`
  the camera at rest, `on_marker_tap` the marker's id; `MapHandle::features_at(point,
  radius)` tests the style's layers top down against the deepest drawn tile's features
  and the app's data (fill by area with holes, line by width, circle by radius), each
  feature once. Unit tests in `query`, headless in `tests/view.rs`.
- [x] Tilt and 3D (after W14): pitch with perspective, extruded buildings, 3D terrain.
  Drawn through the scene3d lane rather than a perspective path of its own: it
  brings the depth buffer, lit materials and every backend including the CPU raster
  the headless pictures use, with no renderer change. `Camera::pitch` (0 to 60°) tilts
  about the focus; `View::to_view`/`to_world` project through the same perspective
  (field of view 2·atan(1/3) over the view's height, ground reached up to three eye
  distances), so gestures, queries, labels, markers and the puck follow it, and the
  covering asks for the tiles the tilted view sees. Flat, nothing changes: the 2D
  tiles, zero upload and zero alloc. Tilted, each drawn tile is raised on
  `viso_ui::work` (`tilt::raise`) into one `ModelAsset` handed to the asset table and
  held by one scene node beneath the overlay (`NodeStore::release_scene_model` lets
  go of tiles that leave): an opaque base, the tile's own drawing lifted over it and
  blended in its order (colors and edge coverage from a small palette texture), and
  `Paint::Extrusion` layers raised into lit solids (`height`/`render_height`, else
  levels × 3 m, else the layer's height; the built-in styles raise their buildings
  10 m). `Style::terrain` names a DEM raster source: a tile waits for its elevations,
  then its base becomes a 32×32 lattice with skirts, the drawing is cut along the
  lattice so each piece lies on one of its planes, slopes turned from the light shade,
  buildings stand on the lowest ground under them and the camera looks at the
  ground's height. Shapes (route, casing, accuracy circle and outline) are raised on
  the loop, each color a step over the one before, the old ones drawn until the new
  are resident; they blend, translucent fills too, their boxes risen far over the
  eye so the scene blends them after every tile's ground. Alt-drag up/down and two fingers sliding together
  tilt, Shift with Up/Down tilts by keys, `N` resets; `MapHandle::tilt_to`,
  `.pitch()`, the pitch camera cell. The navigator drives tilted 50°. Headless
  pictures and pixel checks in `tests/tilt.rs` (fixture tile raised light and dark
  with a route where the projection puts it; buildings flat vs raised; a ridge DEM
  draping and shading the ground; gestures and keys); unit tests in `camera` and
  `tilt`; benches `tile_raise` (Luxembourg z16: 0.32 ms flat, 5.8 ms over relief).
  The ground fades into a haze of the style's background toward the sky (pale blue on
  a light map, deep blue on a dark one): clear to near the top of an unpadded view,
  all haze at the reach and past it, so the ground's ragged far edge never shows, also
  with the focus pushed low by padding (`Lighting::fog_start` lets the scene's fog
  begin at a distance). At most 60° of pitch the horizon stays above the view, so the
  haze is the sky it shows.
  Image layers (raster, hillshade) lie on the tilted ground: each image tile over a
  tile drawn is laid on the worker as a model in that tile's units (a quad, or the
  lattice's own cells over relief, the image cut to the part over the tile, with mips),
  drawn with the layer's opacity; a tile's drawing splits into the part beneath the
  images and the part over them, and every flat part's box spans the tile's square,
  risen a step per order, so the scene's back-to-front blending draws them in the
  style's order, finer images over coarser.
  Open: markers and labels are placed on the plane at the focus's height,
  not on the relief under each.
- [x] Performance: a pan or rotate frame uploads nothing and allocates nothing; tile
  build and label budgets; memory caps; benches beside makepad's map example.
  `tests/alloc.rs` arms a counting allocator around the view's own work (its pointer
  handler and structure hook, through `view::probe`) with vector tiles, images, a
  route, a marker and the puck loaded: a pan or turn frame allocates nothing, except
  a frame that brings a new tile into view and allocates its node. The view tests
  check for zero upload while dragging. Budgets: 12 requests in flight, 192 MB of built
  vector tiles and 64 MB of decoded images a layer (least recently drawn evicted),
  4 ms a label pass. `benches/map.rs` (release, -Oz): the view's work in a pan
  frame takes about 1.2 µs and in a turn frame about 0.8 µs; a real z14 Berlin
  OpenMapTiles tile builds in 5.3 ms on one worker. Makepad's map example has no
  bench to run beside these, and building it was not tried; its tile tessellation is
  timed only by ad-hoc logging inside `geometry.rs`.
- [x] `viso-ext/viso-tiles`: PMTiles v3 (local file and HTTP range reads), MBTiles
  (read-only SQLite), XYZ over HTTP through `viso-net` with a disk cache, offline region
  downloads with progress.
  PMTiles v3 (file, bytes, HTTP range; a byte-capped LRU of leaf directories), MBTiles
  through its own SQLite format reader and writer (no C, passes sqlite3 integrity_check), XYZ
  over `viso-net` with an ETag/max-age disk cache, TileJSON, resumable offline downloads;
  PMTiles lookups take 0.4–1 µs.
- [x] `viso-ext/viso-map-nav`: place and address search with prefix completion and
  categories, a routing graph built from OSM (car, bike, foot, turn restrictions), A*
  routes, turn-by-turn maneuvers, a navigation session (map matching, progress,
  off-route and reroute), elevation profiles from DEM tiles.
  Prefix search with typo tolerance (category words in seven languages, 0.6–1.5 ms), an
  OSM road graph (per mode, one-ways, barriers, turn restrictions), A* with ALT (1–2 ms
  across Luxembourg, within 1–10% of OSRM; contraction hierarchies were measured and not
  taken, having no turn costs), turn-by-turn maneuvers, `NavSession` (map matching, ETA,
  off-route, reroute), elevation profiles from SRTM and terrain-RGB.
- [x] Map data tooling in viso-ext: OSM PBF of a region → a PMTiles base map, a search
  index and a routing graph.
  `viso-map-build` reads PBF with its own parallel streaming reader and writes Shortbread
  MVT (PMTiles or MBTiles), a search index and a road graph, with ocean polygons,
  multipolygons, and per-zoom simplification and merging; Luxembourg takes 3–5 s.
- [x] Example: an offline navigator (search, long-press to route, simulated drive with
  turn banners) run on desktop, mobile and the web.
  `examples/map` runs on the Luxembourg extract built by `viso-map-build` (PMTiles base
  map, search index, road graph), through `viso-tiles` and `viso-map-nav` by path
  `../viso-ext/...`; it is a workspace of its own, excluded from the main one, so the
  main workspace still builds without viso-ext. Search as you type near the camera,
  long-press to drop a pin, Car/Bike/Walk, routing on the worker pool (to the nearest
  reachable point where the place itself is not), Drive simulates the trip at four
  times its pace through `NavSession`, the camera following heading-up and the banner
  showing the next maneuver's arrow, distance and words. Headless test over the real
  data (`VISO_MAP_DATA`): search to a route, long-press to a second, a drive with its
  banner, pictures read. Desktop runs; iOS, Android, the web, Windows and Linux pass
  clippy, not run on devices. Data loads by file on native targets and by URL on the
  web. A real tile of the extract is a committed fixture with its ODbL attribution.
