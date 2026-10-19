# Architecture correction & Embedded — A1 → A18

Implements Architecture Part XXV §73A (Phase 10) and ADR 0041–0044. Order is by
dependency: mechanical moves land before semantic changes (AGENTS §40), and every
section keeps `cargo xtask check-deps`, `cargo check --workspace --all-targets`
and the affected crates' tests green.

## A1 — Remove dead DAG edges (ADR 0042)

- [x] Drop `viso-gpu -> viso-runtime` (Cargo.toml + xtask allowlist)
- [x] Drop `viso-text -> viso-gpu` (Cargo.toml + xtask allowlist)
- [x] `cargo check` of gpu/text (all targets) green after the drop
- [x] xtask: forbidden-edge list carries `gpu -> runtime`, `text -> gpu`, `text -> render`

## A2 — Geometry has one owner: `viso-math` (ADR 0042)

- [x] Inventory every public geometry type outside `viso-math` (render `Rect`/`Point`, svg `Point`/`Rect`/`Transform`, ui `Vec2`, behavior `Vec2F32`/`Vec3F32`, math `DPoint`/`DRect`)
- [x] `viso-render` / `viso-svg` use `viso_math::{Rect, Point, Vec2, Affine2}`; duplicates deleted (render `PathTransform` stays: a translate + uniform-scale placement, not geometry)
- [x] Add `ui -> math`, `widgets -> math`, `behavior -> math`; ui/widgets/behavior geometry uses `viso-math`
- [x] Facade prelude re-exports `Point`/`Rect`/`Vec2` from `viso-math`; `viso::math` module (ui `Size` is a sizing request, not geometry)
- [x] xtask: rule rejecting public `struct Rect|Point|Vec2|Vec3|Vec4|Affine2|Mat2|Mat3|Mat4|Quat` outside `crates/math`
- [x] Platform OS-seam units renamed `LogicalInsets` (f64 logical points, not geometry)

## A3 — Node storage: index links, generation parity, fixed capacity (§14.1, §16)

- [x] Read makepad's tree/area storage for comparison (no retained tree; `Area` = draw-list slot + `redraw_id` staleness)
- [x] `NodeIndex(u32)` with `NONE = u32::MAX`; `NodeLinks` 20 bytes
- [x] Internal links (tree, worklists) store `NodeIndex`; single-link accessors + `children()` replace `links()` on every traversal; layout walks the bare index chain
- [x] Binding target as an offset from the instance's node base (A7 `TemplateBinding` + `InstanceBase`)
- [x] `generation` odd = occupied; drop `occupied: bool`
- [x] `NodeArena::with_fixed_capacity(n)` + `ArenaFull` error; `NodeStore::with_fixed_capacity` / `reserve_nodes`; columns grow together (`node_columns!`, geometric)
- [x] Size assertions (`size_of::<NodeLinks>() == 20`) + 100k create/traverse/remove bench (index vs checked traversal, fixed-capacity create)

## A4 — `viso-schema` and compiler off the runtime stack (ADR 0042)

- [x] Create `crates/schema` (`no_std + alloc`, depends on `viso-ende` only)
- [x] Move `DirtyClass`, `PropertyId`/`EventId`/`ComponentTypeId`/`StyleId`, schema descriptors
- [x] Move UI IR / Binding IR / `PatchBundle` / AOT package format types
- [x] `viso-ui` / widgets export schema tables as data consumed by `viso-dsl`
- [x] `viso-dsl` drops `viso-ui` and `viso-view`; depends on schema/ende/behavior/shader
- [x] Hot-reload commit moves from `crates/dsl/src/hotreload` to `viso-view` dev module
- [x] `viso-ui-macros` emits via schema; `viso-cli` drops `viso-view`
- [x] Shader IR single owner `viso-shader`; dsl hands shader blocks to its frontend
- [x] GPU ABI description types usable without linking a backend
- [x] xtask: transitive-closure rule for dsl/lsp/cli (no runtime/platform/GPU backend)

## A5 — Frame phases and dirty worklists (ADR 0043)

- [x] `FramePhase::ORDER` = 17 phases; driver runs them in order
- [x] `RunContinuations`, `AdvanceAnimations`, `ApplyDevPatches` (dev cfg) feed one transaction
- [x] `RunEffects` and `UpdateSemantics` after `Submit`; flush only queues effects (`EffectStore::run_queued`), writes land next frame
- [x] `EffectStore::cancel_for_node` walks a per-node chain (was a full scan per freed node)
- [x] Per-class dirty worklists (append on 0→1), reused buffers (`DirtyLists`; `clear_dirty` costs the marked nodes)
- [x] Measure/Layout dirty-root reduction + depth order (shallowest first, ancestor flag walk)
- [x] Empty-worklist phases are O(1); idle frame test asserts zero traversal
- [x] Facade runs `ResolveStyle` / `Layout` / `UpdateTransforms` / `BuildPaintChanges` as separate phases; style resolves by worklist cursor (late marks picked up before paint)
- [x] Effect cycles across frames: a chain of `SETTLE_ROUNDS` frames woken only by effect writes is cut and reported `E4202`

## A6 — Transform roots and draw-level shifts (ADR 0043)

- [x] Read makepad's scroll/transform path for comparison (scroll re-lays out and redraws the list; `view_transform`/`view_shift` uniforms exist but scroll does not use them)
- [x] Transform roots: a scroll container's content and a translated node (sticky); paint records them in pre-order (`TransformTable`) and brackets them with `Primitive::Transform`
- [x] Primitives stay world-space as painted; segments carry their root and moving-clip chain (`Space`), never batch across roots; no per-instance transform bytes
- [x] Every built-in reads a per-draw `shift` uniform (MSL/WGSL/HLSL, SPIR-V re-frozen, headless raster); `Renderer::set_transforms` moves draws and clips, uploads nothing; tainted roots (offscreen, backdrop, material, program) answer `NeedsUpload`
- [x] Scroll marks `TRANSFORM` only; a first translate adds `PAINT` once
- [x] On-screen rects derived (`NodeStore::world`), hit test carries the displacement; no `HIT_TEST` on scroll; `world` column and `resolve_transforms` removed
- [x] Culling: paint emits every node of a root, so a scrolled-in node has retained paint; virtual-list mounts paint through structure
- [x] Displacements snap to device pixels (length-env scale factor)
- [x] Bench: `list_100k_scroll` cost independent of visible child count (release: 237 ns at ~30 and at ~160 mounted rows; a frame that paints nothing uploads nothing)

## A7 — Template-level binding tables (§18.1)

- [x] `TemplateBinding { state: Local(offset) | Import(StateId), target: offset, dirty }` + 12-byte `InstanceBase`; value evaluation stays with the node's reader (evaluators are A8's)
- [x] Node and state blocks: `NodeArena::reserve_block`/`alloc_at`/`release_block`, `StateStore::alloc_local`; a freed block slot stays with its block, released blocks reused by length
- [x] `BuildCx::instance(&mut Template, ..)`: the first contiguous instance records the template; matching instances store no edge; others keep owned absolute edges
- [x] Lifetime: an instance lives while a node does; `BindingTable::collect` frees its states, edges and blocks (fixes the recycled-row state/edge leak); templates drop with their last instance
- [x] Regions own sub-templates: one `Template` per arm, every `for` item / arm mount is an instance; `retain_static` scan on every unmount replaced by `collect`
- [x] VirtualList rows share one binding table (imports fan out to every row)
- [x] Absolute edges: per-cell linked slab, O(1) insert/remove (was O(edges) insertion shift); pending write-set dedupe O(1) (was a linear `contains`)
- [x] Memory tests: N instances add N × `InstanceBase` only; recycling rows grows no store

## A8 — Release native lowering (ADR 0041)

- [x] AOT emitter: `aot::lower_module` lowers every chunk (handlers, region entries, shown values, computeds, effects, initializers, functions, closures) → Rust source
- [x] Tasks lower to resumable state machines (`TaskFrame` + `Poll`); `start_task` / `resume_task` drive compiled frames
- [x] Generated code honors VM semantics: overflow, undo log, budgets, fault kind + bytecode location + message (`E7101`–`E7105`)
- [x] `EvaluatorId` = chunk index; handler table entry → chunk shared with Dev; `install_native` keyed by build hash
- [x] `view!` embeds its lowered module (`viso::__behavior!`: release, or any build without `vm`); package path `package_native_code` + `ViewLoad::native`; `ViewHost::with_native`
- [x] Interpreter is the `interpreter` feature (`vm` of `viso-view` / `viso`); without it an uncompiled chunk faults `E7105`
- [x] State mirror sync gated by `StateStore::epoch` + instance revision: a dispatch copies no cell when nothing changed
- [x] `tests/differential.rs` + `differential/*.vs`: every call / task start / resume / `done` handler compared (value or fault, states, revision, dirty, events, starts, cost); caught the task-native resume block bug
- [x] Bench `release_handler_native_vs_vm` (`benches/behavior_native.rs`)

## A9 — Damage-driven present (§30.3)

- [x] Damage from per-entry content keys (store entry + context + sampled pass/capture/material/clock) diffed in order against the last presented list; transform-root moves and clips placed at submit (`viso_render::damage`, `renderer/present.rs`)
- [x] Merge to ≤ 4 disjoint rects; full-frame fallback above 60% of the surface; size / clear / `damage_all()` force whole
- [x] `present_with_damage` on D3D12 (`Present1` dirty rects) / Vulkan (`VK_KHR_incremental_present`) / headless; GLES3 (A11) and CpuRaster (A10) report theirs with their backends; Metal / WebGPU redraw changed frames whole (drawable age unknown)
- [x] Buffer-age aware partial redraw (`frame_age`: D3D12, Vulkan, headless; 4-frame history); `LoadOp::Load` + per-rect clear quad + scissored replay; empty damage skips the frame entirely; pixel-exact vs whole frames (`crates/render/tests/damage.rs`)
- [x] Counters `damage_px`, `damage_rects`, `full_frame_fallbacks` (profiler); `inspect frame` reports the frame's damage for the Inspector overlay

## A10 — `CpuRaster` production backend (§30.4)

- [x] Grow the headless backend into `CpuRaster` (`viso_gpu::cpu`, keeps the headless / golden role; the only CPU backend)
- [x] Tile binning (64×64, submission order), only touched tiles visited (damage costs damage), bands in parallel on a persistent worker pool; output independent of worker count
- [x] Formats: XRGB8888 / ARGB8888 (`Bgra8Unorm`, byte order kept, integer src-over), RGB565 (+ 4×4 Bayer dither for gradients), `f32` HDR, A8 atlases; alpha-less surface → `Bgra8Unorm` intermediates; pipeline blend state honored
- [x] Analytic AA lanes; filled paths as scanline coverage through the mask lane (`Caps::coverage_paths`); A8 glyph / mask cache in system memory
- [x] NEON / SSE2 / SIMD128 kernels, bit-exact against scalar (unit tests; goldens shared)
- [x] Tile-local layers: composited-only offscreen passes deferred, evaluated per tile into worker windows, no target allocated; bit-exact vs whole targets (`crates/render/tests/cpu_raster.rs`)
- [x] `Blitter` capability seam (`viso_gpu::cpu::Blitter`; pass clears; declined work stays on the CPU)
- [x] Custom shaders: every brush packs its CPU program (`ShaderLang::None`, register-compiled interpreter), so none lacks a CPU path; the profile pinning the package's languages is A13
- [x] Benches `cpu_raster_800x480_{full,damage_5pct}_{xrgb8888,xrgb8888_4t,rgb565}` (report `pixel_bytes` / `scratch_bytes` / damage-frame allocations)

## A11 — `Gles3Backend` (§33.1, ADR 0044)

- [x] Read makepad's OpenGL backend (VAO + divisor instancing, UBOs, texture units + sampler objects, flipped offscreen projection, shared-context compile)
- [x] GLSL ES 3.00 codegen in `viso-shader`: built-ins (`emit_glsl`, `ShaderLang::Glsl`) and user programs (`Target::Glsl`), one source per program with the vertex half under `VISO_VERTEX`; literals spelled in their context's type; glslang-checked (`every_builtin_compiles_as_glsl_es_300`, `shader_golden`)
- [x] RHI implementation (`viso_gpu::gles`): EGL/GLES loaded at run time, Wayland / XCB / Android / pbuffer surfaces, uniform ring, texture units + sampler objects for bind groups, program instances in 16 KB uniform chunks, compile on a shared context from any thread; capability profile (no compute, no bindless, no indirect)
- [x] Partial update: `EGL_EXT_buffer_age` → `frame_age`, `EGL_KHR_partial_update` region from the surface pass's scissors, `eglSwapBuffersWithDamage`
- [x] Selected by the `gles` feature (`viso` / `viso-gpu`), `Backend = Gles3Backend` on Linux / Android; `__per_backend!` carries a `glsl` arm

## A12 — `backend/linux/kms` (§48.1.1)

- [x] Read makepad's `platform/src/os/linux/direct` (libdrm legacy `SetCrtc` / `PageFlip` blocking in `select` every frame, GLES2 only, a thread per `/dev/input/event*` drained once a frame, no damage, no CPU path)
- [x] DRM/KMS atomic modeset with a legacy fallback, raw ioctls (no libdrm); GBM + EGL surface (`Scanout::gbm`, libgbm loaded at run time); dumb-buffer double buffering (`Scanout::present_pixels`, per-buffer stale damage)
- [x] Page-flip events pace redraws; `FB_DAMAGE_CLIPS`; hardware cursor (arrow / I-beam / cross, scaled and turned); rotation through the plane (`TEST_ONLY`-probed), else turned while copying (CPU) or drawn turned (`Scanout::turns`, GLES `viso_place` / `viso_frag_position`)
- [ ] Overlay planes for video (no video surface in the platform yet)
- [x] evdev input: keys (xkbcommon, US fallback), mice, encoders → Tab / Shift+Tab, touchscreens (MT protocol B with `EVIOCGMTSLOTS` resync, single-touch), absolute pointers, touchpads; uevent netlink hotplug (no udev daemon)
- [x] Optional libinput adapter (`kms/libinput.rs`, loaded at run time): acceleration, tapping / two-finger scroll, calibration matrix; keyboards and encoders stay on the evdev decoder. Pinch / swipe gestures wait for a gesture `RawEvent`
- [x] Single epoll loop: card / evdev / uevent / seat / waker / timerfd; nothing armed when idle
- [x] Session: libseat (seatd / logind) with pause / resume, else direct nodes with the VT in graphics mode
- [x] Capability declarations: `PlatformApp::capabilities()` → `PlatformCaps` (`FULLSCREEN`: no multi-window / menus / portals / system clipboard / IME); mobile and web declared too

## A13 — Footprint Profiles and `MemoryBudget` (§50A.3–§50A.5)

- [x] Profile → fixed feature set resolution (`viso-project`)
- [x] Profile fixes the package's shader languages (`embedded-cpu` → `None` only); a package without the profile's language fails the build
- [x] `cargo xtask check-profiles` (forbidden items per profile)
- [x] `MemoryBudget` shared by text/image/paint caches + GPU bytes (faces / shaping / glyph atlas sized from it; heap / gpu / images checked per frame, trim then report)
- [x] Accounting allocator (`viso::alloc::Counting`, live + peak bytes); `BudgetExceeded` diagnostic (stderr once per excess, Inspector `budget` group)
- [x] Worker count from profile; `workers = 0` idle-slice execution (§13.3): text, assets and CpuRaster threads
- [x] Footprint benches + Makepad comparison baseline (`cargo xtask footprint`, `benches/footprint/baseline.{json,md}`, §50A.5.1)

## A14 — Embedded fonts and build-time assets (§37.8, §51.1)

- [x] Build-time subset from static strings + declared charset; diagnostic when missing (`packaged_fonts!` reads `VISO_FONT_CHARSET` / `VISO_FONT_TEXT`: glyph-id-preserving glyf/CFF/CID-CFF/collection subset with GSUB + composite closure, missing static characters are a compile error; Text/Font Runtime §3.4)
- [x] `viso build` for an embedded target writes the static strings of `.vs` / `ui!` / `component!` to a file passed as `VISO_FONT_TEXT` and the `Viso.toml` charset as `VISO_FONT_CHARSET`
- [x] Baked A8 atlas; rasterizer-free build when all text is baked (`fonts.bake` / `fonts.rasterizer` → `VISO_FONT_BAKE` / `VISO_FONT_RASTERIZER`: `packaged_fonts!` bakes the subset's drawable glyph closure into aligned "VBA8" pages via the leaf `viso-glyph-a8`, pixel-identical to the runtime rasterizer; the worker decodes baked sizes first; facade feature `rasterizer` with compile-time asserts that every glyph is baked, outline tables stripped, integer resample for undeclared sizes; Text/Font Runtime §3.6)
- [x] Locale-pruned Unicode tables (the Thai/Lao/Khmer/Myanmar line-break LSTMs behind `complex-line-break`; an embedded build adds it only when the device `locales` / catalog locales, static text or charset use those scripts; 327 KB of a Latin build; Text/Font Runtime §3.7)
- [x] Image conversion to target format / ASTC-ETC2-BC; SVG pre-raster option (`viso build` release converts `assets/` per target into a `VTX1` overlay — BC desktop, ASTC mobile, ETC2 embedded-gpu, RGBA8/565/5551/4444 embedded-cpu — with Viso-owned pure-Rust codecs; zero-copy upload, `Caps::sampled` + software-decode fallback; `images.svg = "raster"` drops the SVG lane; Architecture §51.1, Rendering §6.3)

## A15 — MCU-ready invariants (§50A.6)

- [x] `#![no_std]` math / ende / schema / behavior
- [x] text shaping path and ui arena/reactive/layout free of std-only APIs (`Clock` injection)
- [x] No `f64` in hot data
- [x] AOT package XIP: aligned sections, borrowed decode from mapped flash
- [x] CI: `thumbv7em-none-eabihf` smoke build

## A16 — Accessibility seam `SemanticsDelta` (ADR 0042)

- [x] Viso-owned `SemanticsDelta` at the platform seam
- [x] AccessKit conversion inside macOS/Windows/Linux/Android adapters only
- [x] iOS / Web adapters consume `SemanticsDelta` directly

## A17 — Embedded dev loop (§50A.7)

- [x] Desktop emulation of embedded profiles (format, rotation, budgets, workers)
- [x] Remote Dev session over TCP (Ende `PatchBundle`), cross build + deploy

## A18 — Sibling documents

- [x] `Viso_CLI.md`: embedded targets, deploy, remote session, profile emulation
- [x] `Viso_Rendering.md`: transform roots, damage, CpuRaster, GLES3 (§6.8.1–§6.8.2, §8.4, §24.2.1)
- [x] `Viso_Text_Font_Runtime.md`: embedded font policy, text without gpu (§3.4–§3.5)
- [x] `Viso_Hot_Reload.md`: commit in `viso-view`, remote board session (§27A)

## A19 — Incremental paint (Rendering §9.7)

Phase 1 — a frame that only repaints:

- [x] `paint_scale` bench: hover and mount cost against tree size (cards 100 / 2000, rows 30 / 160)
- [x] UI records each node's own drawables on every whole paint (stream span, enclosing root, opacity, flags); a PAINT-only frame re-emits just the dirty nodes and rewrites their primitives in place (`Painted::Patched`), falling back to a whole paint when a node's shape moved
- [x] Renderer `patch`: re-ingest the rewritten primitives at their store slots, re-lower them into their recorded scratch positions and upload only those slots; a whole upload when a precondition fails (kind, count, bounds, resource, backdrop capture)
- [x] Present: a patched entry re-keys its surface draw (through its offscreen pass's composite); when nothing moved since the last present the damage is the rewritten draws alone
- [x] Differential tests: patch and whole upload leave the same scratch, segments, pool shadows, keys and damage
- [x] Facade drives the patch path; footprint and headless scenarios unchanged
- [x] Rendering §9.7 and Architecture describe the patch path with the measured numbers

- [x] Footprint window rows re-recorded on an unlocked screen

Phase 2 — a frame that changes shape:

- [x] Reconcile finds each surviving row in one pass over the window, not a scan of the mounted rows per row
- [x] A row host that re-measures re-places only itself in its absolute-rows canvas, not every mounted row
- [x] A fade patches in place: an opacity change that keeps the node's layer rewrites its layer primitive; the renderer re-tints the offscreen composite or re-folds the group's instances
- [x] Store and pool diffs compare bits, not floats: one integer compare per field, and an unchanged NaN is not a change
- [x] A paint-order entry carries only what is read: bounds are `world` + `paint`, a composite's instance and a backdrop's rect live in per-frame side tables, pass indices are `u32` (entry 216 → 104 bytes, `StoreRef` 64 → 12)
- [x] The stream's primitive shrinks: the rare large variants (frosted material, path, shader, color effect) move out of line (152 → 72 bytes, its widest leaf beside its tag)
- [x] Lowering joins a draw to the open segment without building batch items when kind, bind group, clip, target and space match
- [ ] Surface draw keys and pass sums skip the hash work an unchanged entry already did
- [x] Splice: a subtree whose shape changed replaces its span of the stream, and the renderer re-ingests and re-lowers only that span (a virtual list mounting a row, a text edit)
  - [x] Store entries drop the positional transform/brush ids nothing read
  - [x] Renderer `splice`: runs of leaves in the surface pass outside a folded group splice the paint order and every leaf store, later entries moving their stream index and slot; the frame is re-lowered, synced and keyed; differential tests against a whole upload, store by store, and on present
  - [x] UI records each node's subtree span; a frame whose structure, layout or paint changed in place re-emits only the changed subtrees and splices them into the stream (`Painted::Spliced`)
  - [x] Facade drives the splice path; `paint_scale` mount and a text-edit case measured
  - [x] A patch whose leaf no longer fits its slot (kind, glyph count, bounds, texture) splices it one for one instead of uploading whole
  - [x] A re-measure stops at an ancestor whose size does not follow its content: a flex child of a fixed box, its main request fixed or a fill share, is laid out again alone; a node's own request change (fixed size, fill weight, folded length, hiding) re-places its parent's children; differential test against a whole layout
  - [x] Lowering splices too: inserted leaves lower at the scratch tails and only the tails upload, the segments beside the run lower again with it or are cut, and a whole lowering compacts past 64 lost joins or half the live slots left behind
  - [x] Surface draws and their keys splice too, instead of re-keying the frame: a pass's content mixes each entry's place in the pass, so a pass moved whole keeps it
  - [x] The glyph store splices a run without moving every glyph after it: inserted runs' glyphs append, removed ones' are left behind and packed once they outnumber half
  - [x] A splice stages its damage: the rects the removed draws were shown at and the rects of the inserted draws, the presented rects spliced alongside, so a spliced frame presents as a partial redraw of what it changed
  - [x] A scroll slides the virtual list's window: with clean data the overlap keeps its keys and offsets, so only the rows past either end park and the rows uncovered at either end mount; a jump or a data change rebinds by key
  - [x] A relinked parent's children diff visits only the children the arena logs it touched: their old runs leave, and those that paint there now paint fresh after the last sibling before them that kept its run; an unknown change (`forget_children`, a log past 1024) walks every child
