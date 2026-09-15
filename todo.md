# Viso Rendering — F0 → F4 → D0 → D3 → C0 → E2 → M0 → M1 → A0

Construction order is strict (Viso_Rendering.md §34): each layer's Done gate must be
green and its contract frozen before the next layer builds on it. The immediate-mode
boundary (`Primitive` enum + value types, `upload`/`submit` signatures, `paint.rs`,
`repaint_dirty`, facade prelude) stays frozen through F0–F4.

F0–F4 (the foundation) are complete and frozen. D0 → A0 below are planned but not yet
started; work them strictly in order, one verifiable sub-unit at a time, never letting an
upper layer become a prerequisite of a lower one (§3): `M0/M1/A0` must never be listed as
a completion prerequisite of `D0~E2`; `D0~D3` must never depend on compute; `A0` is
advanced optimization, not a correctness prerequisite. Crate landing points follow the
frozen split (§2): `viso-render` owns geometry / bounds / ROI / retained identity /
instance packing / batching / dirty upload / clip / effect-bounds / backdrop-dependency;
`viso-shader` owns coverage / blur / distortion / color kernels (knows nothing of widget /
node / clip-chain / layout); `viso-gpu` owns buffer / pipeline / command / surface /
texture / dispatch / barrier (knows nothing of rect / text / shadow / glass). No new
crates. Material *semantics* and platform material *parameters* (M0/M1) are out of scope
here — deferred to `Viso_Visual_Materials.md`; only the render/shader/gpu-side backdrop /
ROI / blur / pass / format work belongs in this plan.

Gate for every section: `cargo xtask check-deps` · `cargo fmt --all -- --check` ·
`cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace` ·
`crates/render/tests/golden.rs` · `crates/render/benches/renderer_steady_state.rs`.

---

## F0 — Color / Coverage / Snapping / Legality contract (`math`)

Goal: one definition of color, coverage, snapping, and geometry legality, inheritable by
every layer below `render`. `math` is the home (leaf, pure numeric, reachable by
`text`/`shader`). The wire `Rgba` (straight linear `{r,g,b,a}`) stays byte-identical.

- [x] Read makepad color/premultiply/sRGB↔linear + coverage semantics; confirm current
      viso pipeline: framebuffer = linear premultiplied, wire = straight linear, fragment
      premultiplies, `out = src + dst*(1-src.a)`, AA `edge∈[0,1]` multiplies premul.
- [x] `math/src/color.rs` (NEW):
  - [x] `LinearPremul { r, g, b, a }` — canonical linear-light premultiplied blend rep.
  - [x] `LinearStraight { r, g, b, a }` — straight linear (the wire shape; `Rgba` aliases it).
  - [x] Input spaces: `Srgb`, `DisplayP3`, `LinearSrgb`, `ExtendedLinear`, each with
        `into_linear_premul()` and (where meaningful) `into_linear_straight()`.
  - [x] sRGB↔linear transfer (piecewise: 0.04045 / 12.92 / 1.055 / 2.4) + round-trip.
  - [x] Display-P3 primaries → linear-sRGB matrix.
  - [x] `premultiply` / `unpremultiply` (guard a→0), straight↔premul.
  - [x] Spec guard: no sRGB-space blur/gradient/compositing (types make it unrepresentable).
- [x] `math/src/coverage.rs` (NEW):
  - [x] `Coverage(f32)` clamped `[0,1]` (NaN→0, not `clamp` which would propagate NaN).
  - [x] `composite(premul: LinearPremul, cov: Coverage) -> LinearPremul` = `premul * cov`
        (the §5.6 identity all AA/glyph/path-fringe route through).
- [x] `math/src/snap.rs` (NEW):
  - [x] `PixelSnap { None, Position, Bounds, Stroke }`.
  - [x] device-scale-aware `Hairline` (~1 device px under rotation / non-integer scale / HiDPI).
  - [x] snap helpers keyed by device scale (round position / bounds / stroke to device grid).
- [x] Geometry legality (§5.9/§5.10) — classification at commit/cold boundary only, no
      hot-path `is_finite()`:
  - [x] `GeometryLegality` enum {Ok, NaN, NonFinite, IllegalExtent, SingularTransform,
        Degenerate}.
  - [x] `classify_rect` / `classify_extent` / `classify_transform` (reuse `Affine2`/`Mat*`
        singular guards) → fast-reject, no panic.
- [x] `math/src/lib.rs`: declare `color`/`coverage`/`snap`/`legality` modules + re-exports;
      extend crate docs (color/coverage/snap/legality are F0 contracts, not draw helpers).
- [x] Admit `render → math` into the enforced DAG: add `"viso-math"` to viso-render's
      `allowed_edges()` in `xtask/src/main.rs` and `viso-math` to `render/Cargo.toml`
      (edge points into the leaf, no cycle; matches §3.5 render-above-math).
- [x] `render/src/primitive.rs`: re-home `Rgba` as a view over the `math` straight-linear
      color (`pub use viso_math::LinearStraight as Rgba`; byte-identical `{r,g,b,a}`, all
      literals + `Rgba::TRANSPARENT` keep compiling).
- [x] `render/src/lib.rs`: `Rgba` re-export path unchanged (re-exports from `primitive`).
- [x] Unit tests: sRGB↔linear round-trip; premul/unpremul identity + a→0 guard; coverage
      clamp; `composite` identity; hairline at scale {1,1.5,2,2.75} & 45°; every legality
      case classified; P3→linear sanity. (viso-math: 92 tests green.)
- [x] Gate green (check-deps · fmt · clippy · test --workspace · golden byte-identical after
      `Rgba` re-homing · steady-state bench) → commit F0 (source + this todo, one commit).
- [x] FREEZE F0: color/coverage/snap/legality signatures + linear-premul canonical rep.
      Frozen contract (stable for F1–F4 and D/C/E/M/A to build on):
      - Canonical blend rep: `LinearPremul { r, g, b, a: f32 }` (`#[repr(C)]`, linear-light,
        premultiplied) — the only type `composite`/`over`/`scale` operate on.
      - Wire rep: `LinearStraight { r, g, b, a: f32 }` (`#[repr(C)]`, straight linear);
        `render::Rgba` is this type. `premultiply`/`unpremultiply` bridge the two.
      - Input spaces `Srgb`/`DisplayP3`/`LinearSrgb`/`ExtendedLinear`, each →
        `into_linear_straight()` / `into_linear_premul()`. No sRGB-space blend path exists.
      - `Coverage(f32)` (NaN→0) + `composite(LinearPremul, Coverage) -> LinearPremul`.
      - `PixelSnap`, `Hairline` + `snap_device`/`snap_position`/`snap_bounds`/`snap_stroke_center`.
      - `GeometryLegality` + `classify_rect`/`classify_extent`/`classify_transform`.

## F1 — RHI: generation-safe handles + deferred destruction + device-loss (`gpu`)

Goal: close the largest current-state gap (§6.4–6.8). Handles become generation-safe
`{index, generation}` (stale handle → detectable, never a wrong-object hit; matches §8.2
`NodeId` shape); storage becomes a `SlotMap` (free-list + generation) instead of an
append-only `Vec` indexed by `id.0`; `GpuBackend` gains `destroy_*` + a fence/epoch retire
queue so a destroyed slot is reclaimed only after the GPU is done with it (buffer growth
retires the old slot instead of leaking); a `device_lost()` recovery hook covers
acquire/present/resize/DPI/out-of-date. `gpu` is the home (RHI layer, §17.1). The
`InstanceLayout`/`validate_attrs` §36.1 CPU↔GPU ABI cross-check at pipeline registration is
preserved untouched.

Read makepad first (`viso-read-makepad-first`): `makepad/platform/src/os/apple/metal.rs`
(resource create/write/encode/present, UMA, in-flight frame management) and
`makepad/platform/src/` (resource handle lifecycle, fence/semaphore semantics). Extract
generation / retire / device-loss *behavior*, then implement `SlotMap`/`RetireQueue`/
`device_lost` natively — architecture is Viso's own. No makepad keywords/source comments in
production code.

### F1.1 — Generational handle *shape* (shape-only; byte-identical golden/bench)
- [x] `gpu/src/lib.rs`: change the `resource_id!` macro to emit
      `#[repr(C)] pub struct $name { pub index: u32, pub generation: u32 }` with a
      `pub const fn new(index: u32) -> Self { index, generation: 0 }` and
      `pub const fn index(self) -> u32`. Backends ignore `generation` in F1.1 (always 0).
      Applies to all six: BufferId/TextureId/SamplerId/PipelineId/BindGroupId/SurfaceId.
- [x] `gpu/src/lib.rs`: fix the stale crate doc (l.9–10 "Phase 0 status … No backend
      implementation") and remove the makepad reference in the `GpuInstance` doc (l.99).
- [x] `gpu/src/headless.rs`: every construction `Id(self.<vec>.len() as u32)` →
      `Id::new(self.<vec>.len() as u32)`; every index `self.<vec>[id.0 as usize]` →
      `self.<vec>[id.index() as usize]`.
- [x] `gpu/src/metal.rs`: same conversion (construct + index sites).
- [x] `render/src/lib.rs`: re-export line l.36 `{BindGroupId, PipelineId, TextureId}` still
      compiles (type names unchanged); all `TextureId(..)` → `TextureId::new(..)` across
      render (lib.rs, glyph_atlas.rs, primitive.rs, color_atlas.rs) + ui/viso/widgets;
      inspect dumps use `.index` (BatchId — render's own tuple type — left untouched).
- [x] Gate green + golden/bench **byte-identical** (pure shape change, generation always 0).

### F1.2 — `SlotMap<T>` behind storage (still append-only; green)
- [x] `gpu/src/slots.rs` (NEW): generic `SlotMap<T>` — dense value storage + per-slot
      generation + free-list. `insert(T) -> {index, generation}`, `get(id) -> Option<&T>` /
      `get_mut` (generation mismatch → None, no stale hit), `remove(id) -> Option<T>`.
      F1.2 uses only `insert`/`get` (append-only, free-list unused yet).
- [x] `gpu/src/headless.rs` + `metal.rs`: replace each append-only `Vec<T>` resource store
      with `SlotMap<T>`; `create_*` returns the real `{index, generation}` from `insert`;
      lookups go through `get(id)`. Handles now carry a real generation (no longer always 0).
- [x] `gpu/tests/generation.rs` (NEW): a `SlotMap` returns distinct generations after
      reuse; a stale handle resolves to `None`, never a wrong object.
- [x] Gate green + golden/bench byte-identical (allocation shape unchanged: still one
      insert per create, no per-frame growth).

### F1.3 — `destroy_*` + `RetireQueue` + fence/epoch (deferred reclamation)
- [x] `gpu/src/retire.rs` (NEW): `RetireQueue` + in-flight `Epoch`/`Fence` — a destroyed
      slot is parked with the epoch it was retired in and reclaimed (slot freed for reuse,
      generation bumped) only once the GPU has finished that epoch. `begin_frame`/`present`
      advance the epoch and signal completion. (Fence/in-flight/retire behavior informed by
      the makepad Metal semantics extraction — behavior only.)
- [x] `gpu/src/backend.rs`: extend `GpuBackend` with
      `destroy_buffer/texture/sampler/pipeline/bind_group`, an epoch/fence concept on
      `begin_frame`/`present`, and drive the `RetireQueue`. `create_*`/`write_*`/`encode`
      signatures unchanged.
- [x] `gpu/src/headless.rs` + `metal.rs`: implement `destroy_*` → retire; on epoch
      completion, reclaim into the `SlotMap` free-list (generation bumped so old handles go
      stale). Metal: honor GPU completion via command-buffer completion / fence.
- [x] `render/src/renderer.rs`: buffer growth sites `destroy_*` the old buffer (→ retire)
      before creating the larger one, instead of leaking the old slot.
- [x] **Extend** `crates/render/benches/renderer_steady_state.rs` (never weakened):
      `buffer_count()` stays flat across warmed frames **and** the retire queue drains to
      empty (no unbounded growth of parked slots).
- [x] `gpu/tests/generation.rs`: destroyed slot is reused only after its epoch completes;
      pre-fence reuse is impossible; buffer growth reclaims the old slot without premature
      free.
- [x] Gate green (Metal + headless) + golden/extended-bench.

### F1.4 — `device_lost()` + surface lifecycle (acquire/present/resize/DPI/out-of-date)
- [x] `gpu/src/backend.rs`: make `begin_frame` fallible — `fn begin_frame(&mut self, surface)
      -> Option<Frame>`; `None` means the drawable is unavailable this frame (surface
      out-of-date / drawable pool exhausted), so the caller skips and retries next frame. On a
      failed acquire the epoch does NOT advance (no phantom in-flight frame stalls the fence).
      Add `fn device_lost(&mut self, surface: SurfaceId)` (§6.4): drop any held drawable and
      unblock the retire queue so a stalled fence cannot deadlock reclamation.
- [x] `gpu/src/metal.rs`: `begin_frame` returns `None` when `nextDrawable` is nil (never park a
      phantom epoch — the epoch advances only after a drawable is in hand); `configure_layer_geometry`
      returns a "did change" bool and only re-sets `drawableSize`/`contentsScale` when the physical
      size or scale actually differs (guard against redundant CATransaction / drawable-pool rebuild);
      `resize_surface` drops the held drawable then reconfigures; `device_lost` drops the held
      drawable and advances the fence to the current epoch so parked slots reclaim.
- [x] `gpu/src/headless.rs`: `begin_frame` returns `Some(Frame)` always (a CPU framebuffer is
      never out-of-date); `device_lost` signals the current epoch's fence and drains so a stalled
      queue is freed. `resize_surface` reallocates the buffer.
- [x] `render/src/renderer.rs`: `submit` handles `begin_frame` returning `None` — skip encode +
      present for that frame (the retained scene is unchanged; the next frame redraws it).
- [x] `gpu/tests/`: `device_lost` unblocks a stalled retire queue; a resize changes the surface
      dimensions and reallocates without leaking the old drawable; the `Option<Frame>` call sites
      updated across generation.rs + headless_quad.rs.
- [x] Gate green (Metal + headless) + golden/bench.

### Freeze
- [x] FREEZE F1: the `GpuBackend` trait surface (incl. `destroy_*`/fence/epoch/`device_lost`)
      + the generational `{index, generation}` handle scheme. F2 and F4 bind to these.

## F2 — Compile-time pipeline manifest + typed `GpuPod` ABI (`shader`, `macros`, `gpu`)

Goal: close §7.1 — the release standard draw path must never parse/compile shader
source because a Button first appears. The IR→MSL codegen is already pure and
byte-frozen (`emit_msl` == the `testdata` oracles); F2 lifts it out of the
first-cold-registration path into a **compile-time `PipelineManifest`** whose backend
artifacts (MSL for Metal, builtin tag for headless) are materialized once, not per
first-use. No `build.rs` (a build script re-deriving frozen constants would double
`gpu`'s objc2 compile for no gain — the manifest is a `const`/`OnceLock` value the
oracle tests already prove byte-equal to `emit_msl`). The §36.1 CPU↔GPU
`InstanceLayout`/`validate_attrs` cross-check stays, now run against manifest reflection.
`shader` is the manifest home (owns IR+codegen, edge into `gpu`); `macros` owns the
`GpuPod` derive; `gpu` retires the `PipelineDesc.shader_source` placeholder.

### F2.1 — Scrub production makepad-source comments (no behavior change; byte-identical)
- [x] `gpu/src/instance.rs`: removed the makepad references in the module doc and
      in `validate_against`; kept the §-refs to Viso's own spec and the ABI rationale.
- [x] `macros/src/gpu_instance.rs`: removed the makepad `DrawVars`/`DrawShaderInputs`
      reference comments; derive logic unchanged.
- [x] `macros/src/lib.rs`: removed the makepad `DrawVars` reference in the crate doc.
- [x] `gpu/src/resource.rs`: scrubbed the "Phase 2" version-history comment;
      reworded to describe the field neutrally (§32/§36.1).
- [x] Gate green + golden byte-identical (comment-only edits).
- Note: F2.1 scope is the shader/ABI subsystem (`gpu`, `macros`, `shader`) — now
      clean. ~60 more makepad-keyword / "Phase N" version-history comments remain in
      out-of-layer crates (`platform`/`render`/`text`/`runtime`/`dsl`/…); those are
      each foundation layer's own scrub, not smuggled into an F2 commit (§40).

### F2.2 — `GpuPod` ABI derive (§7.3) enforcing the full typed-layout contract
- [x] `macros/src/gpu_instance.rs` → renamed the derive to `GpuPod` (the §7.3 name);
      still emits the `unsafe impl viso_gpu::GpuPod` + inherent `const LAYOUT`
      + `fn validate_against`. Trait + derive renamed together in `gpu/src/lib.rs`
      (single source of truth — no parallel `GpuInstance`/`GpuPod` split). All ~15
      consumer sites (gpu/backend/instance docs, render primitive + renderer, shader
      msl/codegen docs, viso + ui-macros re-export comments, gpu tests) follow.
- [x] Enforce §7.3 at derive time (compile-fail, not runtime): non-`#[repr(C)]` rejected;
      any field type outside {f32,[f32;2|3|4],u32,[u32;2|4]} rejected at the field span
      via `attr_format()` (the whitelist excludes bool/usize/isize/enum/pointer/reference
      by construction), with an expanded §7.3 diagnostic naming the rejections; every field
      required `Copy` via a generated compile-time `assert_copy::<Self>()` that turns a
      missing `Copy` into a clear derive-site error; no implicit tail padding (all whitelist
      types are 4-byte-aligned ⇒ no inter-field/tail padding, and the §36.1 `StrideMismatch`
      check catches any residual at registration).
- [x] The four instance structs carry `#[derive(GpuPod)]` (`QuadInstance`/`ImageInstance`/
      `GlyphInstance`/`MeshVertex` in `render/src/primitive.rs`). Compile-time
      `size`/`align`/`offset` + backend binding layout + reflection-compat all project
      from `const LAYOUT` (unchanged).
- [x] `gpu/tests/` (trybuild): extended the compile-fail suite — added `not_copy.rs`
      (non-`Copy` struct → `assert_copy` bound error) and `bool_field.rs` (`bool` field →
      §7.3 field-span error), alongside the existing bad-field-type / enum / missing-repr-C
      cases; all `.stderr` regenerated.
- [x] Gate green + golden/bench byte-identical (layout const unchanged; only the derive
      name + diagnostic surface moved).

### F2.3 — `PipelineManifest` (compile-time enumerated standard pipelines)
- [x] `shader/src/manifest.rs` (NEW): `PipelineManifest` — for each of the enumerated
      standard families, one `PipelineEntry { family: PipelineFamily, variant: VariantKey,
      msl: &'static str, schema: InstanceSchema, vertex_entry, fragment_entry }`. The
      `msl` is the frozen `emit_msl` output surfaced as `&'static str` (via the existing
      `msl.rs` `OnceLock` accessors — materialized once, not per first-use); the manifest
      is the single lookup the renderer consumes at device init.
- [x] `PipelineFamily` enum (§7.5): `SolidRect`, `AnalyticRRect`, `AnalyticEllipse`,
      `AnalyticLine`, `Image`, `Gradient`, `PathFill`, `PathStroke`, `MaskComposite`.
      Map the current four builtins (Quad/Image/GlyphRun/Mesh) onto families; families
      with no F2 builtin yet are declared but unpopulated (D-layer fills them).
- [x] `VariantKey` (§7.5 / §7.5-detail): packed integer over ONLY pipeline-changing dims —
      {fixed-function state, resource layout, shader family, sample count, depth/stencil
      class}. Dynamic params (color/radius/opacity/gradient angle) are instance/uniform,
      never a variant. No uber-shader.
- [x] `shader/src/lib.rs`: re-export `PipelineManifest`/`PipelineFamily`/`VariantKey`/
      `PipelineEntry`; add `pub fn standard_manifest() -> &'static PipelineManifest`
      (`OnceLock`-built, the device-init prewarm source per the Impeller-like philosophy).
- [x] Gate green; oracle byte-equality intact (manifest `msl` == `testdata` oracles).

### F2.4 — Renderer + backends consume the manifest; retire `shader_source` placeholder
- [x] `gpu/src/resource.rs`: remove `PipelineDesc.shader_source: &'static str`;
      `PipelineDesc` carries the manifest artifact reference (family/variant + the
      `&'static str` MSL borrowed from the manifest entry) instead of a raw per-call string.
- [x] `render/src/renderer.rs` (l.260-327): the four `create_pipeline` sites consume
      `standard_manifest()` entries (family lookup) rather than `QUAD_MSL()`/`quad_schema()`
      etc. directly; still pass `&QuadInstance::LAYOUT` for the §36.1 cross-check.
- [x] `gpu/src/metal.rs` (create_pipeline, l.382-440): `newLibraryWithSource` compiles the
      manifest artifact's MSL — but this now runs only at device-init prewarm, never on the
      first-Button draw path. Keep `layout.validate_against(&desc.instance_schema)?`.
- [x] `gpu/src/headless.rs`: unchanged builtin-tag path (never compiles MSL).
- [x] Update `PipelineDesc` test sites (`headless_quad.rs` ~l.96-106 with `shader_source:""`).
- [x] Gate green (Metal + headless) + golden/bench byte-identical.

### F2.5 — Zero-copy typed upload (§7.4) + dev-mode manifest source wiring
- [x] Confirm/enforce the §7.4 path for the four instance stores: `&[GpuPod]` → typed byte
      view (`bytemuck`-free `unsafe` cast behind the `GpuPod` `Copy`+no-padding guarantee,
      one `SAFETY:` block) → `write_buffer` mapped range. Audit `renderer.rs` upload sites
      for any `Vec<Instance>→Vec<f32>→Vec<u8>` chain and remove it if present.
- [x] Wire the existing `ShaderPipeline`/`CompiledShader` (`shader/src/reload.rs`) as the
      **dev-mode** manifest source (Viso_Hot_Reload §19-21: shadow-compile candidate IR,
      keep last-good on failure, swap at a safe frame boundary = F1 RetireQueue). Release
      uses the compile-time manifest; dev uses the reload holder. No release hot-path tax
      (§60 / Hot_Reload §1: no PatchBundle engine / dev transport in release).
- [x] `render/tests/` (NEW): **release-no-runtime-compile** — a first-Button paint through
      the Metal path triggers zero `newLibraryWithSource` (instrument the backend with a
      compile counter; assert it is 0 after device-init prewarm across the first frame).
- [x] Gate green + `metal_glyph.rs` green + §36.1 cross-check runs against manifest reflection.

### Freeze
- [x] FREEZE F2: the `PipelineManifest` shape, `PipelineFamily`/`VariantKey`, and the
      `GpuPod` ABI of the four instance structs (`QuadInstance`/`ImageInstance`/
      `GlyphInstance`/`MeshVertex`). F3 (stores/instances) and F4 (instance pool/upload
      ring/batch `BatchKey`) bind to these.
      Frozen contract (stable for F3–F4 and D/C/E/M/A to build on):
      - `PipelineFamily` (`shader/src/manifest.rs`): `SolidRect` / `AnalyticRRect` /
        `AnalyticEllipse` / `AnalyticLine` / `Image` / `Gradient` / `PathFill` / `PathStroke` /
        `MaskComposite` (§7.5). Dynamic params ride the instance/uniform data, not pipeline
        permutations — no uber-shader, no per-instance variant explosion.
      - `VariantKey { family, color_target: ColorTargetClass, sample_count: u8, depth_stencil:
        bool }` with `packed() -> u32` over disjoint bit fields (family 0..8, color 8..16,
        samples 16..24, depth bit 24). Only dimensions that truly change pipeline/state/code
        are variant keys.
      - `PipelineEntry { family, variant, builtin: BuiltinShader, msl: &'static str, schema:
        InstanceSchema, vertex_entry, fragment_entry }`; `PipelineManifest { entries }` with
        `.entries()` / `.entry(family)`; `standard_manifest() -> &'static PipelineManifest`
        (`OnceLock`, the four implemented built-ins: SolidRect→Quad, Image, MaskComposite→
        GlyphRun, PathFill→mesh). Manifest MSL is the frozen `*_MSL()` oracle byte-for-byte.
      - `GpuPod` instance/vertex ABI (`render/src/primitive.rs`, `#[repr(C)]`, align 4,
        pinned in `render/tests/instance_abi_frozen.rs`):
        - `QuadInstance` stride 56: rect_pos@0, rect_size@8, color@16, radius@32,
          border_width@36, border_color@40.
        - `ImageInstance` stride 48: rect_pos@0, rect_size@8, uv_pos@16, uv_size@24, color@32.
        - `GlyphInstance` stride 48: same shape as `ImageInstance`, frozen independently.
        - `MeshVertex` stride 28: pos@0, color@8, edge@24 (per-vertex, not per-instance).
      - Zero-copy upload (§7.4): `&[GpuPod]` → typed byte view → mapped `write_buffer`; no
        `Vec<Instance>→Vec<f32>→Vec<u8>` chain. Release never compiles MSL on a draw
        (`render/tests/metal_no_runtime_compile.rs`); dev keep-last-good stays byte-identical
        to the frozen manifest (`render/tests/dev_shader_pipeline.rs`).

## F3 — Retained scene under frozen immediate-mode API (`render`)

Goal: make the renderer retained *internally* (§8) while `upload<B>(&mut self,
backend, &[Primitive])` / `submit<B>` keep their exact frozen signatures and
`primitive.rs`'s value types, `paint.rs`, `repaint_dirty`, and the facade glob do
not move. Today `upload` clears every scratch Vec and re-lowers the whole
`&[Primitive]` slice each frame (immediate mode, `renderer.rs:410-418`). F3 keeps
that ingest boundary byte-identical but routes it through retained per-type stores
addressed by typed generational IDs, with separate revision planes so a paint-only
change re-lowers nothing geometric. Positional identity is sound because
`repaint_dirty` (`ui/src/component.rs:1981`) re-emits the whole tree in stable
pre-order every frame — the same determinism makepad relies on for slot reuse,
but Viso keys stable `PrimitiveId`s and per-plane revisions rather than a single
`redraw_id`. New modules land under `crates/render/src/scene/` (render owns the
retained scene; no new crate). The retained/diff API exposed *to* `ui` is a later
stage (F3-ext), out of scope here.

### F3.1 — Shadow store (pure addition; immediate walk still authoritative)
- [x] `render/src/scene/ids.rs` (NEW): typed dense generational IDs — `PrimitiveId`,
      `TransformId`, `BrushId`, `ClipId`, `ImageId`, `GeometryId`, `PathId`, `MeshId`,
      `ClipChainId`, `EffectChainId`, `MaterialId`, `RenderChunkId`, `PaintChunkId`
      (§8). Each is `{index: u32, generation: u32}`, reusing the F1 `SlotMap` shape
      (`gpu/src/slots.rs`) but render-local (scene IDs are positionally assigned +
      diffed, not free-listed like GPU resources). No pointer/`usize` identity.
- [x] `render/src/scene/revision.rs` (NEW): separate monotone revision planes —
      `GeometryRevision`, `PaintRevision`, `TransformRevision`, `ClipRevision`,
      `ResourceRevision`, `EffectRevision`, `VisibilityRevision` (§8.4). Each a
      `u64` bump counter; a paint-only change bumps `PaintRevision` only.
- [x] `render/src/scene/store.rs` (NEW): per-type compact stores (dense SoA/AoS, NOT
      `Vec<Box<dyn>>`) — `SolidQuadStore`, `ImageStore`, `GlyphRunStore`,
      `VectorPathStore`, `MeshStore`, `ClipStore`, and identity-separated
      `TransformStore`/`BrushStore` (§8, §8.5). Hot fields compact; cold fields in
      side tables. Fixed-capacity, cleared not freed across frames (high-water reuse,
      like the existing `quad_scratch`).
- [x] `render/src/scene/bounds.rs` (NEW): `local`/`world`/`clip`/`paint`/`effect`
      bounds where `paint_bounds = geometry + stroke inflation + filter inflation`,
      computed without re-parsing paths (§8). Feeds F4 visibility/chunking later.
- [x] `render/src/scene/mod.rs` (NEW): retained `Scene` aggregating the stores +
      revision planes + bounds; becomes an internal field of `Renderer`.
- [x] Retessellation cache: move `Path::tessellate` into `VectorPathStore` keyed by
      `PathId` + quality bucket, so transform/color changes reuse the cached
      tessellation (§8). F3.1 populates it alongside the immediate walk.
- [x] `render/src/renderer.rs` `upload`: keep the immediate walk producing scratch as
      today, but ALSO build the retained stores from the same `&[Primitive]`, then
      re-derive instances from the stores and assert byte-identical to the scratch
      the immediate walk produced (shadow test — pure addition, no source-of-truth
      change). Behind a debug-only assertion path so release carries no cost (§60).
- [x] `render/tests/` (NEW): shadow byte-identity — for `test_scene`, the store-derived
      `QuadInstance`/`ImageInstance`/`GlyphInstance`/`MeshVertex` bytes equal the
      immediate-walk scratch bytes, instance-for-instance, in paint order.
- [x] Gate green + golden/bench byte-identical (F3.1 adds stores, changes no output).

### F3.2 — Ingest-diff (stable IDs by positional identity; revision-plane bumps)
- [x] `render/src/scene/ingest.rs` (NEW): walk `&[Primitive]` assigning a stable
      `PrimitiveId` by positional identity vs the previous frame (Nth primitive of a
      kind → same slot). Diff each primitive field-wise against the retained store;
      bump ONLY the affected revision plane(s) — a moved quad bumps `TransformStore`/
      `TransformRevision`, a recolored quad bumps `PaintRevision`, unchanged →
      zero store mutation (§8.4, this is "0 primitive reconstruction" under a
      whole-tree re-emit).
- [x] Kind/count-change handling: when positional identity breaks (a primitive kind
      changes at a slot, or the tree grows/shrinks), reslot on the cold "structure
      changed" path (§9.5) — no per-frame HashMap on the steady-state path.
- [x] Identity separation verified: a pure move dirties `TransformStore` only;
      geometry/tessellation and `PaintRevision` untouched (§8, §11).
- [x] Extend `FrameStats` (today `{draw_calls, instances}`, `renderer.rs:153-162`)
      toward §61 with integer counters (no alloc): `visible_primitives`,
      `dirty_primitives`, `quad_instances`, `glyph_instances`, `path_tessellations`.
- [x] `render/tests/scene_diff.rs` (NEW): paint-only color change bumps `PaintRevision`
      only — `GeometryRevision`/`TransformRevision`/`ClipRevision`/`ResourceRevision`
      unchanged. Steady-state: identical input re-uploaded → zero plane bump,
      `dirty_primitives == 0`, visible primitive still counted.
- [x] Gate green + golden/bench byte-identical (diff drives the same store contents).

### F3.3 — Switch source of truth (submit walks retained stores)
- [x] `render/src/renderer.rs`: `submit`/`upload` lower from the retained stores +
      revision planes instead of the per-frame immediate scratch; remove the
      top-of-`upload` scratch clear (`renderer.rs:410-418`) and the immediate walk,
      keeping the store-derived lowering that F3.1 proved byte-identical. Segments/
      draw-list building unchanged (F4 replaces segment merge with the batch planner).
- [x] `render/src/scene/inspect.rs` or extend `render/src/inspect.rs` (§62): expose
      `PrimitiveId` → instance/segment ranges, cold-path only (no steady-state cost).
- [x] Gate green + golden byte-identical F3.1→F3.3 + steady-state bench: identical
      input → zero store mutation, allocations flat, `buffer_count` unchanged.

### Freeze
- [x] FREEZE F3: the typed generational scene IDs, per-type store layout, the seven
      revision planes, and the bounds set (`local`/`world`/`clip`/`paint`/`effect`).
      F4 (instance pool / upload ring / coalescer / batch planner / render chunk)
      binds to these. Machine-enforced in `render/tests/scene_contract_frozen.rs`
      (one consolidated gate: id shape + typed-id set + revision planes + bounds set),
      alongside the per-module unit tests in `ids.rs`/`revision.rs`/`bounds.rs`.
      Frozen contract (stable for F4 and D/C/E/M/A to build on):
      - Typed scene handle (`render/src/scene/ids.rs`): `SceneId { index: u32,
        generation: u32 }` — `#[repr(C)]`, 8 bytes, align 4, mirroring
        `viso_gpu::slots::RawId` (one identity discipline across the stack, §8.2).
        `new(index)` is generation 0 (first positional assignment); the diff bumps
        `generation` only on the cold structural reslot path. Every typed id is a
        `#[repr(transparent)]` newtype over it with `new`/`index`/`generation`:
        `PrimitiveId` / `TransformId` / `BrushId` / `ClipId` / `ImageId` /
        `GeometryId` / `PathId` / `MeshId` / `ClipChainId` / `EffectChainId` /
        `MaterialId` / `RenderChunkId` / `PaintChunkId`. No pointer/`usize` identity.
      - Revision planes (`render/src/scene/revision.rs`): `Revisions` — exactly seven
        independent `u64` bump counters (56 bytes, no padding): `geometry` / `paint` /
        `transform` / `clip` / `resource` / `effect` / `visibility` (§8.4). Each
        `bump_*` is `wrapping_add(1)` and moves that plane alone; a consumer snapshots
        the set and rebuilds only when a plane it depends on advanced. A paint-only
        change advances `paint` and nothing else.
      - Per-type stores (`render/src/scene/store.rs`): dense AoS/SoA `Vec` entries
        (never `Vec<Box<dyn>>`), positionally slotted, persisting across frames —
        `begin_frame` resets a cursor without freeing, `finish_frame` trims the tail
        the walk did not revisit. `SolidQuadStore` (`QuadEntry { instance, transform,
        brush }`), `ImageStore` (`ImageEntry { instance, texture }`), `GlyphRunStore`
        (`GlyphRunEntry { start, count, atlas }` + shared instance/prev buffers),
        `VectorPathStore` (`PathEntry { path, quality, vertices, indices }`, in-line
        retessellation cache keyed by geometry + quality bucket), `MeshStore`
        (`MeshEntry { vertices, indices }`), and identity-separated `ClipStore`
        (`ClipEntry { rect }`) / `TransformStore` (`TransformEntry { origin }`) /
        `BrushStore` (`BrushEntry { color }`). Each `ingest_*` diffs field-wise and
        returns `DirtyPlanes { geometry, paint, transform, resource, appended }` — the
        planes the change moved (an all-`false` result is a byte-identical slot: zero
        mutation, zero bump). `StoreRef` (`Quad`/`Image`/`GlyphRun`/`Path`/`Mesh`/
        `Composite { instance, pass }`) tags the paint-order record's per-emit slot.
      - Bounds (`render/src/scene/bounds.rs`): `Bounds` — exactly five `Rect` stages
        `local`/`world`/`clip`/`paint`/`effect` (§8). `Default` is five `Rect::ZERO`.
        `from_world(world, clip, stroke, filter)`: `clip = world ∩ clip` (or `world`),
        `paint = world.inflate(stroke * 0.5 + filter)` (half the stroke bleeds outside
        the fill, filter inflates further), `effect = paint` until a neighbourhood
        effect grows it. Computed numerically, never by re-parsing a path; a
        transform-only change recomputes `world` onward from the cached `local`, a
        paint-only change recomputes nothing. F4's dirty coalescer/visibility read
        `paint`.

## F4 — Persistent data path: instance pool / upload ring / coalescer / arena / batch / chunk (`render`)

Goal: the performance foundation (§9, §36) built UNDER the frozen `upload<B>` /
`submit<B>` boundary. Today (post-F3) `lower_from_scene` still rebuilds five scratch
Vecs from the paint-order spine every frame and each `upload_*` helper does one full
`write_buffer(buf, 0, all_bytes)` (grow via `destroy_buffer`+`create_buffer`); the
draw-list merge is adjacent-only run-length coalescing on `(kind, clip, target)` via
`segments.last_mut()` (`renderer.rs` quad merge / `push_mesh_segment`). F4 makes the
data path persistent: a stable `PrimitiveId → InstanceSlot` in a long-lived device
buffer so a hover (one `PaintRevision` bump on one slot, F3) uploads ONE small range,
not the whole scene; transient uploads recycle through F1's `Epoch`/`Fence`/
`RetireQueue`; dirty slots coalesce into a few `write_buffer` ranges; per-frame chunk/
batch/clip scratch bump-allocates from a frame arena reset O(1) at frame end; and the
segment merge is replaced by an order-safe packed-integer `BatchKey` planner that
preserves F3 §8.6 paint order. All new modules land under `crates/render/src/` (render
owns the data path; no new crate). The frozen immediate-mode boundary, `primitive.rs`
value types, `paint.rs`, `repaint_dirty`, and the facade glob do not move.

Design deltas over the makepad reference (reference-only, never copied): makepad keys a
single `redraw_id` per draw list and reuses one MTLBuffer per draw item *positionally*
by slot index; Viso keys a stable `PrimitiveId → InstanceSlot` and per-plane revisions
(F3), so a local change touches exactly its slot(s). makepad has no instance ring
(realloc-on-still-bound); Viso rotates transient uploads through the F1 fence/epoch ring.
makepad's batch merge is an O(n) backward linear scan with a 1-byte lane key and a
global accumulating z-float; Viso packs a full `BatchKey` integer and maximizes
contiguous compatible runs within paint order without a global z coupling.

### F4.1 — Frame arena (bump scratch; foundation the rest allocate from)
- [x] `render/src/frame/arena.rs` (NEW): a bump allocator for per-frame scratch —
      visible chunk list, batch scratch, clip scratch, small pass descriptors, and
      coalescer/radix scratch (§9.4). Fixed-capacity backing (grown only on the cold
      "frame bigger than ever seen" path, high-water like `quad_scratch`), O(1) reset
      at frame end (bump cursor → 0, backing retained). No per-primitive `Vec`/`Box`/
      `String`/`HashMap` on the steady-state path (§9.4, §28). Typed sub-allocations
      (`alloc_slice::<T>(n)`) with alignment; the raw-pointer bump is module-
      encapsulated with `SAFETY:` comments (§27).
- [x] `render/src/frame/mod.rs` (NEW): frame-scoped state aggregate (the arena)
      owned by `Renderer`; `new`/`reset` lifecycle.
- [x] `render/tests/frame_arena.rs` (NEW): two warmed frames bump-allocate the same
      byte total and reset to cursor 0; a frame that fits the high-water mark performs
      zero heap allocation (counting allocator).
- [x] Gate green + golden/bench byte-identical (arena is scratch plumbing, no output
      change yet).

### F4.2 — Persistent instance pool (draw-order shadow diff)
- [x] `render/src/pool/instance_pool.rs` (NEW): a long-lived, grow-only device buffer
      (F1-allocated via `create_buffer`) per instance family that shares a pipeline and
      stride (quad / image / glyph instance streams + the mesh vertex / index streams)
      (§9.1). The pool keeps a CPU **shadow** of its device buffer's live prefix
      (`shadow[i]` = the value in device slot `i`), created lazily on the first non-empty
      frame. Slot `i` = draw-order element `i`: because F3's sole primitive producer
      re-emits the whole tree in stable order every frame, an unchanged primitive keeps
      its slot, so no separate `PrimitiveId → slot` map is needed. Element bound relaxed
      to `Copy + PartialEq + 'static` POD (not the frozen `GpuPod`) so the `u32` index
      stream — not an instance schema — is a valid element without extending the F2 ABI.
- [x] `sync(backend, &lowered)` diffs the freshly lowered draw-order array against the
      shadow element-by-element and uploads only the maximal runs of changed slots, one
      `write_buffer` per run; returns the write count. Unchanged frame → 0 uploads;
      one-slot repaint → 1 minimal upload; empty frame → shadow cleared, buffer retained.
      Growth (instance count first exceeds capacity) rounds capacity to a power of two,
      retires the old device buffer through F1 `destroy_buffer` (deferred/epoch-reclaimed,
      not leaked), and forces one full upload. Alloc-free steady state (shadow reused).
- [x] Wire the renderer (`renderer.rs`): five pools (quad / image / glyph instances,
      mesh vertex, mesh index) replace the eager per-family buffer+capacity pairs.
      `lower_from_scene` still lowers the whole paint order into reused scratch Vecs;
      the upload step is now five `pool.sync(backend, &scratch)` calls. `command_for`
      reads `pool.buffer()` for each segment's `instance_buffer`; draw-order contiguity
      is preserved (`instance_offset = seg.start * STRIDE`, `index_offset = seg.start`).
- [x] `render/tests/instance_pool.rs` (NEW): first non-empty `sync` creates the buffer
      and uploads once; an identical frame uploads zero; a one-slot change uploads exactly
      one run and leaves other slots untouched; separated changes split into two runs,
      adjacent changes coalesce into one; an empty frame clears live slots without
      destroying the buffer; exceeding capacity grows once (`buffer_count` +1, new buffer
      identity, old retired) and re-syncs silently; the `u32` index stream diffs likewise.
- [x] Gate green + golden byte-identical (same instance bytes reach the GPU, now via the
      persistent shadow-diffed pools).

### F4.3 — Fence-recycle correctness (the transient path has no consumer; the pool is it)
The original plan carried a Frame Upload Ring (§9.2) for "transient" per-frame uploads.
Investigating the post-F4.2 architecture shows there is NO transient buffer-upload path
to serve: the ONLY `write_buffer` consumer in `render` is now the persistent
`InstancePool` (all five streams — quad/image/glyph instances + mesh vertex/index — go
through shadow-diffed pools that upload ZERO on an unchanged frame, strictly better than
a ring that re-uploads every frame). Uniforms ride inline by value in `DrawCommand`
(`InlineUniforms`, no device buffer, no `write_buffer`); offscreen layers reuse a texture
pool, not a buffer. A ring would be dead code with no producer. So F4.3 is collapsed to
verifying the fence-recycle contract the pool already relies on — the performance- and
safety-relevant behavior §9.2 actually cares about — rather than building an unused ring.
- [x] No `pool/upload_ring.rs`: the persistent pools + inline uniforms + texture pool
      already satisfy §9.2's intent (no per-frame buffer realloc; fence-safe reclaim of
      the only buffers that ever churn — a pool's old buffer on grow). Nothing to wire.
- [x] `render/tests/fence_recycle.rs` (NEW): drives `InstancePool` grow against a real
      `HeadlessRaster` across `begin_frame`/`present` cycles and proves the F1 fence gate
      through the backend's observable surface (`retired_count`, `buffer_count`):
      growing retires the old buffer into the `RetireQueue` (parked, `retired_count` +1)
      but does NOT free it in the frame it was retired in (in-flight — the fence has not
      reached that epoch); the next `begin_frame` after that frame is presented reclaims
      it exactly once (`retired_count` back to 0), never earlier (no premature free) and
      never left parked (no leak); `buffer_count` (cumulative creates) is unaffected by
      reclamation; several grows in one frame all reclaim together one frame later; a
      steady frame parks nothing; and device loss drains a stalled queue rather than
      leaking it. The `gpu_pod_bytes` §9.2 `SAFETY` encapsulation stays confined to the
      pool module (untouched by this step).
- [x] Gate green + golden byte-identical (no production code change — pure test +
      todo rescope) + steady-state bench: `buffer_count` flat, retire queue drains.

### F4.4 — Dirty range coalescer (few `write_buffer` ranges, not many micro-copies)
- [x] `render/src/pool/coalescer.rs` (NEW): `coalesce(dirty, out)` merges a strictly
      increasing dirty-slot list into a few contiguous `Range { start, len }` ranges
      (§9.3). Output goes into a caller-owned reused `Vec` (high-water-backed, no
      steady-state heap alloc — §9.3); each range is emitted by the pool as one
      `backend.write_buffer(buf, offset, bytes)`. A clean gap of at most `GAP_THRESHOLD`
      (=4) slots is bridged (one range re-uploading a few clean slots beats many tiny
      copies); a wider gap splits into a new range. 8 unit tests cover the arithmetic.
- [x] Wire the pool → coalescer → `write_buffer`: the within-capacity `sync` path
      collects changed slot indices into a persistent `dirty_scratch`, runs
      `coalescer::coalesce` into a persistent `range_scratch`, and uploads each range as
      one contiguous write. An unchanged frame produces no dirty slots, no ranges, and
      zero `write_buffer` calls for that family. Both scratch Vecs are reused across
      frames (no steady-state alloc). Grow path still forces one full upload.
- [x] `render/tests/coalescer.rs` (NEW): 7 integration tests driving the pool via
      `HeadlessRaster` — one dirty slot → one range; no dirty slots → zero ranges;
      changes within the gap threshold merge into one range; changes past it split into
      two; three scattered clusters collapse to three ranges; repeated scattered
      repaints issue a stable range count (scratch reuse). The rewritten F4.2
      `separated_changes_split_near_changes_merge` also confirms the pool routes through
      the coalescer under the new threshold.
- [x] Gate green + golden byte-identical + steady-state bench (Success on both `upload`
      and `frame`): bridged clean slots re-upload their existing shadow value, so the
      final GPU buffer contents are unchanged (golden byte-identical), while a hover-style
      one-slot change uploads exactly one minimal coalesced range (proves §9.1 + §9.3).

### F4.5 — Order-safe batch planner (packed `BatchKey`, unifies the merge decision)
- [x] `render/src/batch/planner.rs` (NEW): `BatchKey` — a packed `u64` (no strings,
      §16.2, §29) over the full §9.6 field set `{ pipeline family, variant, blend, sample
      count, color-target class, depth/stencil class, render target, resource table }`.
      Only family / render-target / resource carry a value today; the rest are reserved
      bit-fields written as `0` so a later blend/MSAA/depth dimension packs into the
      existing layout without moving a frozen field. `joins(prev, next)` is the single
      adjacency predicate: equal key + structural clip match + mergeable family. The
      three former segment-merge sites now route their merge DECISION through
      `planner::joins`; the goal is the MAXIMUM contiguous run of compatible instances
      within correct paint order (§8.6), NOT fewest draws — adjacency-only, cross-span
      reordering deferred (no reorder-safe spans minted yet).
- [x] `render/src/batch/chunk.rs` (NEW): `RenderChunk { key, family, clip, geometry,
      order }` + `RenderChunkId(u32)`. A LEAN cold-path §62 introspection projection over
      the segment stream — NOT a hot-path parallel structure and NOT a Segment
      replacement. `Segment` stays the sole hot-path draw carrier; `render_chunks()` is
      produced only when §62 tooling asks. `geometry` is `(start, count)` in the family
      buffer (instances for quad/image/glyph, indices for mesh); `order` is the half-open
      paint-order span the chunk absorbed — the one piece an `InspectBatch` cannot carry,
      so a consumer maps a changed paint position back to the one chunk to rebuild.
- [x] `inspect.rs` (§62): `render_chunks()` / `render_chunk(RenderChunkId)` fold
      `segments_snapshot()` for key/geometry/clip/resource and compute the order span via
      `chunk_order_spans()` — routed through the same `joins` predicate, so chunks line
      up 1:1 with segments (`debug_assert_eq!(segments.len(), order_spans.len())`) and
      with `inspect_batches().batches[i]`. Chunk key carries the REAL bound resource, so
      distinct textures yield distinct keys. Zero-geometry entries (empty path/mesh/glyph)
      emit no segment and are skipped, keeping span count == segment count.
- [x] `render/tests/batch_planner.rs` (NEW): a same-key adjacent quad run collapses to
      one batch / one chunk spanning all paint positions; an unmergeable primitive
      between two equal-key quads is a hard barrier keeping them in submission order (no
      cross-barrier merge); `BatchKey` packs/unpacks losslessly across every family /
      target / resource; `render_chunks()` is 1:1 with `inspect_batches()` (matching key /
      geometry / clip, `RenderChunkId(i)` resolves to `chunks[i]`); distinct textures
      yield distinct chunk keys.
- [x] Complete `FrameStats`/§61: `gpu_upload_bytes` (coalescer range sizes via
      `InstancePool::last_upload_bytes`) and `batches` (segment count) wired; `draw_calls`
      / `instances` already present. Extend `inspect.rs`/§62: `BatchId` → pipeline /
      resources and `RenderChunkId` → ranges both live, cold path only.
- [x] Gate green + golden byte-identical + `InspectBatches::dump()` byte-identical +
      steady-state bench: two warmed frames identical alloc (target 0 heap),
      `buffer_count` flat.

### Freeze
- [x] FREEZE F4: `BatchKey` (bit layout + field set), the `joins` adjacency predicate,
      `RenderChunk` shape, the instance-pool upload discipline, and the dirty-range
      coalescer contract — before D0. D/C/E/M/A build on these. Machine-enforced in
      `render/tests/data_path_contract_frozen.rs` (one consolidated gate: family tags +
      mergeability, target field encoding, three-dimension non-aliasing, `joins`
      adjacency, `RenderChunk` field set + `open`/`absorb` growth, coalescer `Range` +
      `GAP_THRESHOLD` bridge/split, pool grow-only / zero-upload / one-slot-upload),
      alongside the per-module unit tests in `planner.rs`/`chunk.rs`/`coalescer.rs`/
      `instance_pool.rs`.
      Frozen contract (stable for D/C/E/M/A to build on):
      - Packed batch key (`render/src/batch/planner.rs`): `BatchKey(u64)` — one packed
        integer, never a string (§9.6). Three live dimensions occupy disjoint bit ranges
        so a key uniquely identifies its `(family, target, resource)` triple and
        adjacency merges on key equality alone: family bits `0..3`
        (`FAMILY_MASK 0b111`), render target bits `14..24` (10 bits, `0x3ff`), bound
        resource bits `24..48` (24 bits, `0xff_ffff`). The reserved §9.6 dimensions
        (variant `3..4`, blend `4..8`, sample `8..10`, color-target `10..12`,
        depth/stencil `12..14`, and `48..64`) are held at 0 — bit-fields exist so a later
        dimension widens without moving a live field. `pack(family, target, resource:
        Option<BindGroupId>)` (resource packs `bg.index`, `debug_assert` on overflow) /
        `family()` / `target_field()` / `resource_field()` / `bits()`. `BatchFamily`
        low-three-bit tags are frozen: `Quad = 0` / `Image = 1` / `GlyphRun = 2` /
        `Mesh = 3`; `mergeable()` is true only for `Quad` and `Mesh`. `BatchTarget` packs
        `Main → 0` and `Offscreen(i) → i + 1`.
      - Adjacency predicate (`render/src/batch/planner.rs`): `joins(prev, next)` is the
        single merge gate every site routes through — true iff `prev.mergeable &&
        next.mergeable && prev.key == next.key && prev.clip == next.clip`. Any one
        differing is a hard barrier; merge is adjacency-only, never across an
        intervening non-joining item (paint order preserved, §8.6).
      - Render chunk (`render/src/batch/chunk.rs`): a cold-path introspection projection
        over the paint-order segment stream carrying exactly `{ key: BatchKey, family:
        BatchFamily, clip: Option<Rect>, geometry: (u32, u32), order: (u32, u32) }`.
        `RenderChunkId(u32)` is a transparent handle. `open(key, family, clip,
        geom_start, count, order_start)` starts `geometry = (geom_start, count)` and a
        one-wide `order = (order_start, order_start + 1)`; each `absorb(count)` extends
        `geometry.1 += count` and the order span by one position. Lean by design — no
        bounds/effect-dep/revision fields until a consumer needs them.
      - Dirty-range coalescer (`render/src/pool/coalescer.rs`): `Range { start: usize,
        len: usize }` (`size_of == 2 * usize`, §9.3). `coalesce(dirty, out)` takes a
        strictly-increasing slot list and writes contiguous upload ranges into reused
        scratch (clears `out`, no steady-state alloc): clean gaps of at most
        `GAP_THRESHOLD == 4` bridge into one range, a wider gap splits. Empty dirty →
        zero ranges; one dirty slot → one minimal one-slot range (the hover case).
      - Instance pool (`render/src/pool/instance_pool.rs`): a grow-only device buffer
        (`InstancePool<T>`, `T: Copy + PartialEq`, `STRIDE = size_of::<T>()`) diffed
        against a CPU shadow — slot `i` is element `i`, inheriting F3's positional store
        order (no separate slot table). `sync(backend, instances)` returns the
        `write_buffer` call count: an unchanged frame diffs to nothing (0 writes, 0
        `last_upload_bytes`); a one-slot change uploads exactly one slot's bytes in one
        write; a grow (cold path, `next_power_of_two`) retires the old buffer via the F1
        retire path and forces one full upload. Capacity is a high-water mark — a shorter
        frame never shrinks it. Contiguous dirty slots route through `coalesce` into few
        `write_buffer` ranges, never per-slot micro-copies (§9.1).

---

## D0 — First complete visible renderer (§10)

The first end-to-end visible renderer, built on the frozen F0–F4 core: retained scene
(F3) diffed into instance pool + coalescer + order-safe batch planner (F4), submitted
through the generational RHI (F1) with build-time pipelines (F2). Scope is deliberately
minimal — Solid Rect only — so the whole visible path is proven before shape variety
(D1) lands. Every §30 counter is wired here and never removed. All primitive landing is
`viso-render` unless marked otherwise.

### D0.1 — SolidRect primitive path
- [ ] Shared unit quad + typed `SolidRectInstance` + `SolidRect` pipeline family (§10.1):
      one global 4-vertex / 6-index quad, N compact instances, one instanced draw — never
      four per-rect vertex buffers, never one vertex buffer per rect. `viso-shader` owns
      the SolidRect coverage program (shortest branch-free fast path, no über-shader);
      `viso-gpu` owns the pipeline/buffer/surface.
- [ ] Affine2 transform per rect (`viso-math` Affine2 → instance field).
- [ ] Primitive opacity multiplied straight into premultiplied color (F0 linear-premul
      canonical rep; not a group-opacity layer — that is C0).
- [ ] SrcOver blend baseline through the fixed-function path.
- [ ] Rect scissor (axis-aligned clip → hardware scissor; the D0 clip form only).
- [ ] Surface present: acquire → encode → present through the F1 RHI, exercising resize /
      device-scale / surface-recreate paths already frozen in F1.

### D0.2 — Hot-path structural zeros (§10.2)
- [ ] Steady-state frame path is structurally: 0 heap alloc per primitive, 0 string
      lookup, 0 global HashMap per primitive, 0 per-primitive backend virtual dispatch,
      0 shader compile (build-time only, F2), 0 full-scene upload (local change → coalesced
      slot upload only, F4). Enforced by the extended steady-state bench, not just asserted.

### D0.3 — §30 perf counters (wired from D0, never removed)
- [ ] Wire the full §30 counter set into `FrameStats`/§61 as integer counters (no alloc):
      `visible_primitives`, `culled_primitives`, `render_chunks`, `batches`, `draw_calls`,
      `pipeline_switches`, `texture_binding_switches`, `uploaded_bytes`, `uploaded_ranges`,
      `instance_rebuilds`, `path_tessellations`, `clip_mask_builds`, `offscreen_passes`,
      `transient_target_bytes`, `blur_pixels`, `backdrop_capture_pixels`,
      `shader_pipeline_creations`, `cpu_render_build_time`, `cpu_encode_time`,
      `gpu_frame_time`. D0 populates the ones it exercises; later layers light up the rest.
- [ ] Effect Cost Metadata scaffolding (§30): the enum
      `Local / Analytic / NeedsMask / NeedsOffscreen / NeedsBackdrop / DestinationRead /
      ComputePreferred` exists so D-layers tag primitives; Inspector cost fields + dev-only
      Debug Overlay are stubbed (cold path, strippable in release, §60).

### D0.4 — §31 acceptance / benchmark gate
- [ ] Acceptance scenarios (§10.3): 1 / 10k / 100k rect; large scrolling list;
      single-hover dirty; window resize; DPI change; surface recreate. Record per scenario:
      CPU build/encode time, uploaded bytes, draw calls, pipeline switches, alloc count,
      GPU frame time.
- [ ] Benchmark gate (§31 `## D0/D1`): 10k / 100k Rect; scroll transform-only (no paint
      rebuild); hover paint-only (one instance-range write, same pipeline/geometry/clip/
      batch). High-refresh regression profile at 60 / 120 / 144 / 240 Hz.

### D0 Done
- [ ] Solid Rect + SrcOver + Scissor.
- [ ] Local dirty does not full-upload.
- [ ] 100k rect benchmark repeatable.
- [ ] Hot path structurally zero per-primitive heap allocation.

### Freeze
- [ ] FREEZE D0: `SolidRect` primitive path (shared-quad + instance layout + pipeline
      family), the §30 counter set + Effect Cost Metadata enum, and the D0 clip/blend/
      present contract — D1 extends the instance/shader tiers on top of these without
      reshaping the SolidRect fast path. Machine-enforce the counter set + instance layout
      in a `render` contract test.

---

## D1 — Analytic UI shapes / border / line (§11)

Mainstream UI shapes as shared-quad + compact-instance + **analytic coverage** (no default
CPU tessellation): animating size/radius/color patches instance fields only. Shapes land
in `viso-render` (identity/geometry/bounds/alignment); coverage programs in `viso-shader`;
pipeline/buffer in `viso-gpu`. Inherits the D0 hot-path zeros and §30 counters.

### D1.1 — Analytic shape primitives
- [ ] `AnalyticRRect` + per-corner radius, with a unified radius-normalize algorithm when
      per-corner radii exceed available size (§11.2; individual widgets must not self-clamp).
- [ ] `RoundedSuperellipse`.
- [ ] `Circle` and `Ellipse`.
- [ ] `Capsule`.
- [ ] Analytic AA correctness proof (coverage routes through F0's single
      `composite(premul, coverage)`; golden vs baseline).

### D1.2 — Border / line
- [ ] `RectBorder` / `RRectBorder` with alignment `Inside / Center / Outside` and
      stroke-consistent bounds inflation (§11.3). Border coverage stays analytic
      (outer/inner distance) — ordinary rect/rrect border never enters the D3 general path
      stroker (§13, boundary confirmed).
- [ ] `Line` with cap `butt/round/square`, join `miter/bevel/round`, `miter_limit` (§11.4).
- [ ] `Circle/Ellipse Stroke`.

### D1.3 — Tiered analytic shader families (§11, build-time enumerated)
- [ ] `viso-shader` tiers: A `SolidRect` / B `RRect·Circle·Ellipse` / C `Fill+Border+
      Gradient` / D `Expanded quad + analytic simple shadow` / E `specialized custom
      analytic primitive`. Rationale: the commonest primitive must not pay the costliest
      primitive's ALU / register pressure / branch cost. Pipeline families enumerated at
      build time (F2 manifest); dynamic color/size/hover stay instance data — no über-shader.
      SDF/analytic scope is limited to rrect / circle / ellipse / capsule / line / arc /
      simple icon / border / simple inner-outer shadow (§7.3); arbitrary SVG is not required
      to become SDF.

### D1.4 — §31 gate
- [ ] Benchmark gate (§31 `## D0/D1`): 10k RRect; mixed Rect/RRect/Circle; scroll
      transform-only; hover paint-only. Rounded-card target: RRect fill + border + one
      shadow → 1 analytic primitive, not multi-level offscreen. High-refresh 60/120/144/240.

### D1 Done
- [ ] RRect / per-corner / Circle / Ellipse / Capsule.
- [ ] Border / Line / Cap / Join basics.
- [ ] analytic AA correctness.

### Freeze
- [ ] FREEZE D1: the analytic-shape instance layouts + per-corner radius normalize, the
      border-alignment + bounds-inflation contract, the line cap/join/miter contract, and
      the A–E shader-tier enumeration. D2 adds brush/image on top; complex path stroke is
      deferred to D3.

---

## D2 — Brush / gradient / image (§12)

Fill/brush model (solid, gradients, image patterns) + image/sprite/atlas lane with
explicit color-space / sampling / edge / resource-caching policy, reusing one image
pipeline family. GPU-internal color is premultiplied alpha (F0). Brush model + resource
policy in `viso-render`; gradient interpolation + sampling kernels in `viso-shader`;
texture/sampler/pipeline in `viso-gpu`.

### D2.1 — Brush model
- [ ] `Brush` enum in `BrushStore` (§12.1 / §10): `Solid`, `LinearGradient`,
      `RadialGradient`, `SweepGradient`, `ImagePattern`, `ShaderBrush`.
- [ ] Gradient stop tiers (§12.2): 2-stop → inline instance colors; small stop count →
      compact shared stop table; many stops / expensive interpolation → cached 1D Gradient
      LUT Atlas. LUT key ≥ {stop colors, stop offsets, interpolation space, extend/tile
      mode, target color-profile class}. Static gradient built once; never recreate a
      gradient texture per frame.
- [ ] Gradient interpolation color-space policy is explicit (§12.3) — never accidentally
      decided by texture format.

### D2.2 — Image / sprite / atlas lane
- [ ] `Image` / `ImageRect` with source rect, destination rect, `fit`, `alignment`,
      `opacity` (§12.4).
- [ ] Sampling `Nearest` / `Linear` / `MipmapLinear` (where available/appropriate).
- [ ] Edge behavior `Clamp` / `Repeat` / `Mirror` (§12.5); atlas/sprite sampling prevents
      adjacent-texel bleeding.
- [ ] Interned `SamplerId` (§12) — image sampling never creates a per-widget sampler.
- [ ] Resource-policy routing (§12): small immutable UI image → atlas candidate;
      large/frequently-replaced → standalone texture; video/camera → external texture (via
      a platform image path, not the standard image path); heavy downscale → mipmaps.
- [ ] `NineSlice`, then `Tile`/`TiledImage`, then `Sprite`/`TextureAtlas` region — added
      only after basic `ImageRect` is stable, reusing the Image pipeline family (§12.6); no
      independent high-cost pipeline.

### D2.3 — §31 gate
- [ ] Benchmark gate (§31 `## D2`): many gradients; image grid; sprite atlas;
      texture-binding pressure. Steady state: 0 gradient-LUT rebuild unless the gradient
      changed; no per-primitive texture creation. High-refresh 60/120/144/240.

### D2 Done
- [ ] Linear / Radial / Sweep Gradient.
- [ ] Image / ImageRect / Sampling.
- [ ] NineSlice / Tile / Atlas.

### Freeze
- [ ] FREEZE D2: the `Brush` enum + gradient-stop tier strategy + LUT key, the image/sprite/
      atlas lane + sampling/edge policy + interned `SamplerId`, and the resource-policy
      routing. D3 adds general paths on top.

---

## D3 — Path / bezier / fill / stroke (§13)

General vector paths with compact storage, both fill rules, and a full stroke contract via
a **retained tessellation cache** so transform/color/opacity changes never retessellate.
Path storage + retained geometry identity + stroke style in `viso-render` (heavy flatten/
tessellate/preprocess dispatched to a worker thread, §26); SIMD candidates in `viso-math`/
`viso-render` with a scalar oracle; GPU geometry buffers in `viso-gpu`. Compute-based
vector is **not** here — it is a large-dynamic-workload lane (A0), and D0~D3 must never
depend on compute (§7.2).

### D3.1 — Path storage & commands
- [ ] `PathArena` (§13.2): `tags` (compact command stream) + `points` (tightly packed f32)
      — never per-segment vtable/Box objects.
- [ ] Commands `move_to` / `line_to` / `quad_to` / `cubic_to` / `close`; `Conic`/`Arc`
      lower to canonical segments (§13.1).
- [ ] Path-creation-time metadata (§13): bounds, segment count, convexity hint,
      simple-shape-recognition hint, complexity score — computed once, never re-scanned per
      render.
- [ ] Fill rules `NonZero` and `EvenOdd` (§13.3).

### D3.2 — Retained tessellation (vector mesh lane)
- [ ] Stable-path default lane (§13.4): `Path → worker flatten/preprocess → tessellate →
      retained GeometryId → cached GPU geometry`. Geometry separated from color/opacity;
      transform-only never retessellates; scale retessellates only past a flatness/quality
      bucket, with **hysteresis** to avoid zoom-threshold jitter.
- [ ] 16-bit index for small geometry, else 32-bit; static device-local geometry is not
      mixed into the frame upload ring (§13, F4 boundary).

### D3.3 — Stroke
- [ ] Stroke contract (§13.5): `width`, `alignment` (where semantically supported), `cap`,
      `join`, `miter_limit`, `dash_array`, `dash_offset`, `hairline`.
- [ ] General path stroke: `PathRevision + StrokeStyleRevision → worker preprocess →
      retained stroke geometry` — reprocess only when path or stroke geometry changes; a
      color change never rebuilds stroke geometry.

### D3.4 — SIMD & SVG input
- [ ] SIMD candidates (§13.6): bounds, flatness evaluation, segment transform, rect
      intersection, point classification, stroke preprocessing — scalar implementation kept
      as the correctness oracle.
- [ ] SVG lane (§13): `SVG bytes → parse → normalized vector scene → Path/Brush/Stroke →
      cached Render IR`. SVG is an input format, not a per-frame XML DOM renderer; static
      assets may pre-parse at build time; runtime dynamic parse goes to a worker.

### D3.5 — §31 gate
- [ ] Benchmark gate (§31 `## D3`): small stable SVG-like paths; large static path scene;
      path transform-only (0 retessellate); stroke/dash heavy; path churn. Steady state:
      0 path tessellation when geometry unchanged; 0 general path parse. High-refresh
      60/120/144/240.

### D3 Done
- [ ] Path commands.
- [ ] NonZero / EvenOdd.
- [ ] Fill / Stroke / Dash.
- [ ] retained tessellation cache.
- [ ] transform/color does not rebuild geometry.

### Freeze
- [ ] FREEZE D3: `PathArena` storage + command set + creation metadata, fill rules, the
      retained-tessellation vector-mesh lane (GeometryId cache + hysteresis + index-width +
      upload-ring separation), the stroke contract + revision-driven stroke-geometry cache,
      and the SVG-input → cached-Render-IR lane. C0 composes clip/mask/group/blend over the
      full D0–D3 primitive set.

---

## C0 — Clip / mask / group / blend (§14)

The compositing foundation before any effect: cost by tier, never defaulting to an
offscreen layer. Clip/mask/composition logic + cache keys in `viso-render`; blend/mask
shader variants in `viso-shader`; `ClipMaskAtlas` / R8 target allocation in `viso-gpu`.

### C0.1 — Clip ladder / Clip Planner
- [ ] Clip ladder (§14.1): axis-aligned Rect → merged hardware scissor; simple RRect /
      simple analytic shape → analytic clip when profitable; complex path → stencil or
      mask; stable repeated complex clip → retained `R8 ClipMaskAtlas` / cached realization.
- [ ] Border radius does **not** imply clip-children; ordinary containers default
      overflow-visible; a scroll viewport gets a scissor, not an offscreen layer just
      because a parent is rounded (§14).

### C0.2 — ClipChain (retained)
- [ ] `ClipChain` retained (§14.2): `ClipChainId → pre-resolved clip descriptor`. With
      geometry unchanged: no path reparse, no mask re-raster, no per-primitive clip-stack
      tree walk. Nested axis-aligned rects pre-intersected. Complex ClipMask key ≥
      {geometry revision, effective transform bucket, device scale, fill rule, clip
      composition}.
- [ ] Empty clip → immediate subtree reject (§14.3).

### C0.3 — Mask
- [ ] Baseline `AlphaMask` / `LuminanceMask`; detailed `ImageMask` / `PathMask` (§14.4).
      Stable mask → independent mask cache. Storage: R8 where possible / tight ROI / tile-
      page allocation; never a permanent full-screen RGBA texture; RGBA only when color is
      truly needed.

### C0.4 — Group opacity
- [ ] Primitive opacity vs group opacity distinguished (§14.5). Primitive opacity
      multiplies straight into premultiplied color. Group opacity uses isolation/offscreen
      **only** when per-primitive opacity is not semantically equivalent (overlapping
      children whose compositing result must be preserved); when unsure, keep correctness
      and use a layer.

### C0.5 — Blend
- [ ] Baseline `Clear/Src/Dst/SrcOver`, then `Plus/Multiply/Screen/Overlay/Darken/Lighten`
      (§14.6). Full Porter-Duff (`DstOver, SrcIn/DstIn, SrcOut/DstOut, SrcATop/DstATop,
      Xor`) + artistic (`ColorDodge, ColorBurn, HardLight, SoftLight, Difference, Exclusion,
      Hue, Saturation, Color, Luminosity`). Destination-read / isolation blends are recorded
      with a `LayerReason` and **deferred to the Effect Planner (E2)** — they must not
      pollute the common `SrcOver` pipeline. Blend fallback path: fixed-function → backend
      destination-read/subpass/framebuffer-fetch → bounded offscreen composite.

### C0.6 — §31 gate
- [ ] Benchmark gate (§31 Matrix): deep Rect clip; mixed RRect clip; complex cached clip;
      nested opacity; blend stress. High-refresh 60/120/144/240.

### C0 Done
- [ ] Rect / RRect / Path clip ladder.
- [ ] ClipChain retained.
- [ ] Mask.
- [ ] Group opacity.
- [ ] Blend baseline.

### Freeze
- [ ] FREEZE C0: the clip ladder + Clip Planner tiers, the retained `ClipChain` +
      ClipMask key, the mask model + R8/ROI storage discipline, the primitive-vs-group
      opacity contract, and the blend baseline + `LayerReason` recording. This also seeds
      the Effect Planner's facts (scissor fast path, mask cache) that E0/E1/E2 consume.

---

## E0 — Analytic shadow (§15)

Common UI shadows get an analytic fast lane so they never default to mask → full-texture
blur → composite. Analytic shadow shaders + DecoratedShape pipeline in `viso-shader`;
instancing/coverage integration + mask/blur caching in `viso-render`; blur targets in
`viso-gpu`. Depends on C0 frozen.

### E0.1 — Analytic shadow fast lane
- [ ] Fast analytic shapes `Rect / RRect / Circle / Ellipse / Capsule` via expanded
      instance quad + analytic distance + Gaussian-like coverage approximation (§15.1) —
      explicitly not the default mask/blur/composite path. Simple shadow does not create a
      blur layer.
- [ ] Parameters `offset`, `sigma`/blur-radius semantic, `spread`, `color` (§15.2).

### E0.2 — DecoratedShape fusion (benchmark-gated)
- [ ] Post-benchmark, a fused `DecoratedShape` pipeline drawing `shadow + fill + border`
      in one primitive family (§15.3). Hard constraint: pure Rect keeps its shorter
      pipeline — never force all rects into a large shader. One-draw-or-not decided by
      shader register pressure / overdraw benchmark.

### E0.3 — Path shadow fallback & inner shadow
- [ ] Arbitrary path shadow (§15.4): `Path/Mask → tight shadow mask → blur → offset/color
      composite`, caching the unshaded blur mask keyed on {same geometry, same sigma},
      reused when only color/offset change. (Full ROI/blur infrastructure is E1; E0's
      fallback is minimal and leans forward to E1.)
- [ ] Inner shadow (§20.3): simple analytic geometry → direct distance function; general
      path → mask/filter lane.

### E0.4 — §31 gate
- [ ] Benchmark gate (§31): 1k analytic shadows; path-shadow reuse. High-refresh.

### E0 Done
- [ ] UI shape analytic shadow.
- [ ] inner shadow.
- [ ] path shadow fallback.

### Freeze
- [ ] FREEZE E0: the analytic-shadow fast lane + parameters, the (benchmark-gated)
      DecoratedShape fusion contract, and the path-shadow blur-mask cache key + inner-shadow
      routing. Feeds the Effect Planner's "can use analytic shadow?" check.

---

## E1 — Offscreen / blur / ROI / transient targets (§16)

Only now the *complete* offscreen infrastructure — ROI, adaptive blur, transient-target
pool, RenderGraph — driven by real demand (no premature general RenderGraph before this).
RenderGraph + Transient Target Planner + ROI in `viso-render`; blur kernels in
`viso-shader`; transient texture pool / memoryless attachments in `viso-gpu`. Depends on
C0 + E0 frozen; builds on F1 fence/retire and F4 pool/batch.

### E1.1 — Tight ROI
- [ ] Every offscreen effect first computes `content/effect bounds ∩ clip bounds ∩ surface
      bounds` (§16.2). Forbidden pattern: small panel → full-screen copy → full-screen blur
      → crop back. Required: `effect bounds + kernel expansion + clip/intersection → tight
      ROI`. Never blur an entire 4K surface for a small widget.

### E1.2 — Blur ladder
- [ ] Auto-select by effective sigma / ROI / backend (§16.3): small → direct/separable;
      medium → optimized separable / compute where profitable; large → downsample pyramid /
      multi-scale (Kawase-like) / upsample. Thresholds are benchmark params, not public ABI.

### E1.3 — Transient Target Planner
- [ ] Lifetime analysis + size/format/sample compatibility + alias-reuse + frame-local pool
      (§16.4) instead of per-effect `create_texture`/`destroy_texture`. Pool keyed by
      format / usage / size-class bucket / sample count. Never one texture per shadow /
      per clip / per material surface.

### E1.4 — Demand-driven RenderGraph
- [ ] Passes emerge from real needs (main / mask / shadow-blur / offscreen-group) then
      abstract into a graph (§16.1, §25). RenderGraph owns pass dependency, resource
      read/write usage, barrier/state lowering, transient lifetime, attachment
      compatibility, pass-merge opportunity — never widget tree / layout / state binding /
      font fallback. Reuse the compiled plan when topology is unchanged; param changes (blur
      sigma / color / transform) must not rebuild topology.
- [ ] Tile-GPU awareness (§16.5): minimize render-target switches / full-screen
      intermediates / store-load cycles / transparent overdraw; allow transient/memoryless
      attachments where the backend supports them; fuse passes only when it lowers real
      bandwidth. Backend-specialized without forcing desktop into a tile model.

### E1.5 — §31 gate
- [ ] Benchmark gate (§31): small / medium / large blur; many small-ROI blurs. Resource
      gate records transient render-target peak bytes + steady occupancy. Static/idle scene
      does not rebuild clip/shadow/gradient cache and does not continuously submit.
      High-refresh.

### E1 Done
- [ ] offscreen ROI.
- [ ] blur ladder.
- [ ] transient target reuse.
- [ ] RenderGraph compile/reuse.

### Freeze
- [ ] FREEZE E1: the tight-ROI computation contract, the blur ladder selection (thresholds
      internal), the Transient Target Planner pool keys + alias-reuse discipline, and the
      RenderGraph responsibilities + compile/reuse (topology-only recompile) contract. E2's
      backdrop / shared-pyramid / Effect Planner build directly on these.

---

## E2 — Backdrop / color effects / advanced blend (§17)

Destination-dependent effects expressed through the RenderGraph dependency graph — never
by reading undefined framebuffer state. Backdrop dependency + MaterialGroup + Effect Damage
in `viso-render`; ColorTransform / advanced-blend / blur-pyramid shaders in `viso-shader`;
capture / shared-pyramid targets in `viso-gpu`. Depends on E1 frozen (+ C0/E0 facts).

### E2.1 — Backdrop capture & sharing
- [ ] Backdrop is an explicit "depends on already-drawn content behind it" semantic (§17.1):
      goes through a RenderGraph dependency; a widget never reads current framebuffer
      undefined state; capture only the required ROI.
- [ ] Shared backdrop (§17.2): multiple Frosted/Glass over the same region → shared capture
      + shared blur pyramid where compatible + multiple material composites. Forbidden
      default: N widgets = N full-screen captures + N blurs. Allow union ROI / shared
      backdrop pyramid / MaterialGroup / shared effect pass.

### E2.2 — Color effect fusion
- [ ] Color effects (§17.3): `Brightness, Contrast, Saturation, HueRotate, Grayscale,
      Sepia, Invert, ColorMatrix, Tint`. Consecutive compatible effects compile into a
      single ColorTransform / matrix-like op; e.g. `Brightness→Contrast→Saturation` must
      not produce three RT passes when math-mergeable. Only a non-expressible custom filter
      gets an extra pass.

### E2.3 — Advanced blend isolation
- [ ] The destination-read blends deferred from C0 (§2577) land here as Nonlocal, isolated
      through the Effect Planner — not on the common `SrcOver` pipeline.

### E2.4 — Effect Planner & Local/Nonlocal classification
- [ ] Local effect (§3102): no neighbor/dest read (`opacity, tint, color matrix,
      brightness, contrast, saturation, simple gradient, simple mask, certain blend states`)
      → fuse into the existing draw shader.
- [ ] Nonlocal effect (§3120): needs neighbor samples / previous framebuffer / group
      isolation (`blur, backdrop blur, large/general shadow, destination-dependent advanced
      blend, displacement, group opacity with overlapping children`) → only then consider
      offscreen/render target.
- [ ] Effect Planner replaces saveLayer abuse (§3145): each potential layer records
      `LayerReason ∈ {GroupOpacity, ImageFilter, BackdropFilter, AdvancedBlend, Isolation,
      ComplexMask, SnapshotCache, NativeMaterialBoundary}`; the planner tries in order to
      eliminate it (push opacity into children? fuse color matrix? scissor instead of clip
      layer? analytic shadow? share backdrop? collapse adjacent effects?) — offscreen is
      created only when all fail. "Offscreen is an expensive mechanism, not a convenient
      default." The planner accretes facts across C0/E0/E1 and is exercised fully here.
- [ ] Effect Damage (§3202): backdrop/blur does not look only at its own property dirty —
      when content behind its ROI changes, `BackdropDependencyRevision` dirties only that
      material/effect's ROI; other backdrops are unaffected.

### E2.5 — §31 gate
- [ ] Benchmark gate (§31): shared backdrop; color-effect fusion. Static-UI target: an
      existing blur/glass/shadow does not cause continuous redraw; idle → 0 GPU submit.
      High-refresh 60/120/144/240.

### E2 Done
- [ ] backdrop dependency.
- [ ] shared backdrop / blur.
- [ ] color effect fusion.
- [ ] advanced blend isolation.

### Freeze
- [ ] FREEZE E2: the backdrop-dependency + shared-capture/pyramid contract, the color-effect
      fusion rule, the advanced-blend isolation, the Local/Nonlocal classification + Effect
      Planner `LayerReason` elimination order, and the Effect Damage `BackdropDependency
      Revision` model. This closes the D0~E2 render foundation; M0/M1/A0 build strictly on
      top and are never a prerequisite of anything below.

---

## M0 — Generic frosted / glass material (§18)

A generic frosted/glass material composed **entirely by reusing frozen E1/E2** — a material
layer, not a renderer foundation. Only the render/shader/gpu-side composition belongs here;
material *semantics* / *parameters* are deferred to `Viso_Visual_Materials.md`.

### M0.1 — Frosted composition (reusing E1/E2)
- [ ] Frosted pipeline composed from frozen lower layers: `backdrop → blur →
      saturation/tint → optional noise → material mask → border/highlight` (§18) — reusing
      E2 backdrop, E1 blur/ROI, E2 color transform, C0 mask, E0 shadow/border; no new
      foundation.
- [ ] Backdrop dependency + ROI in `viso-render`; blur/distortion/color kernels in
      `viso-shader`; texture/pass/sync in `viso-gpu`.
- [ ] Shared-capture requirement: N material widgets must not = N full-screen captures + N
      blurs; support union ROI / shared backdrop pyramid / MaterialGroup / shared effect
      pass (reuses E2's sharing).

### M0.2 — Deferred to Viso_Visual_Materials.md (out of this plan)
- [ ] Material *parameters* + platform *semantics* + full Apple Liquid Glass / Frosted /
      native material lane. Not implemented in viso-render/shader/gpu; only the E1/E2 reuse
      contract above stays here.

### M0 Done (no standalone spec block; validated against E-layer contracts)
- [ ] Frosted composition reuses E1/E2 with shared capture/blur (no per-widget full-screen
      capture). Governed by global DoD: "multiple backdrop/material can share capture/blur";
      "local color effects can fuse".
- [ ] M0 is not listed as a prerequisite of D0~E2.

---

## M1 — Platform material / Liquid Glass / HDR (§19)

Platform-advanced materials last: native/GPU material lanes + HDR/wide-gamut, without
leaking platform-private APIs into the generic Render IR.

### M1.1 — Material lanes
- [ ] Two selectable lanes: `Native System Material Lane` / `GPU Material Lane` (§19), chosen
      by system integration / visual consistency / composability / animation / performance /
      whether Viso GPU content must participate. Platform-private material APIs stay behind
      the Native lane — never in the generic Render IR.

### M1.2 — HDR / wide-gamut
- [ ] Distinguish at least `SDR sRGB target / wide-gamut target / HDR target` (§19); the
      RenderGraph Planner (E1) picks the intermediate format for blur/glass/gradient/blend
      per real need. Forbidden: all offscreen forced to RGBA16F; HDR scene degraded to 8-bit
      sRGB mid-pipeline then re-upped. Builds on F0 color contract + F1 surface format +
      E-layer effect correctness. Never reach down and mutate the frozen F4 instance model
      for a material-specific layout.

### M1 Done (no standalone spec block)
- [ ] Governed by global DoD: "HDR/wide-gamut intermediate not wrongly degraded"; "GPU
      canonical alpha is premultiplied". Not a prerequisite of D0~E2; enters only after M0
      freezes. Material semantics deferred to `Viso_Visual_Materials.md`.

---

## A0 — Advanced GPU optimization (§20)

Compute vector / bindless / indirect / GPU culling as **workload specialization** — never a
correctness prerequisite for ordinary UI, and never a workaround for an inefficient basic
renderer. Compute/coverage kernels in `viso-shader`; ROI/binning/cull-orchestration/batch
metadata in `viso-render`; descriptor tables / indirect / multi-draw / dispatch / barriers
in `viso-gpu`. Enters only after M1 freezes; algorithms here are not frozen as public ABI.

### A0.1 — Vector compute lane (§20.1)
- [ ] Entered only when benchmark proves benefit (large dynamic path / vector editor /
      canvas / complex clip scene / high path churn / CPU tessellation is the bottleneck).
      Concept pipeline: `Path segment scene → tile/binning → parallel prefix/allocation →
      per-tile coverage → fine raster`. The specific algorithm is not public ABI.
- [ ] Hard rule: compute is workload specialization — 20 ordinary buttons / a panel / a few
      dozen stable paths must never be routed through compute dispatch just for architectural
      uniformity (§7.2, §20.1).

### A0.2 — Bindless (§20.2)
- [ ] Capability detection: Metal argument/resource tables / D3D12 descriptor heap / Vulkan
      descriptor indexing / WebGPU binding arrays where available. Fast path exposes
      `instance.texture_index` to avoid texture-change batch breaks. Fallback when
      unavailable: atlas / small texture set / binding-group batching. Public paint API is
      unchanged across backends.

### A0.3 — GPU culling / indirect (§20 detailed, §24.1)
- [ ] Normal UI: CPU bounds + clip intersection suffices. Large canvas/scene: chunk-level
      CPU cull + optional GPU cull / indirect draw / multi-draw / GPU-driven batching. Hard
      rule: 50 UI nodes must never produce compute dispatch for "GPU-driven".

### A0 Done (no standalone spec block)
- [ ] Governed by global DoD: "compute vector only as a large-dynamic-workload lane";
      "default UI does not globally enable MSAA"; plus the 120/144/240 Hz regression gate and
      the unsafe/SIMD reference + benchmark requirement. Not a prerequisite of D0~E2. D0~D3
      must remain correct with zero compute dependency (§7.2).

---

## Global 1.0 Definition of Done (§33 — the render runtime contract)

Not a construction stage; the full-stack acceptance the layers above collectively satisfy.
Verify once the relevant layers are frozen:
- [ ] Rect/RRect/Circle/Ellipse/Line/Arc/Path/Image/Text/Mesh each have an explicit
      primitive path.
- [ ] Solid/Linear/Radial/Sweep/ImagePattern/Shader brushes have explicit semantics.
- [ ] Stroke cap/join/miter/dash/alignment have explicit semantics.
- [ ] Simple UI shape defaults to analytic/instanced, not CPU tessellate.
- [ ] General stable path can be retained cached geometry.
- [ ] Compute vector only as a large-dynamic-workload lane.
- [ ] Default UI does not globally enable MSAA.
- [ ] Rect clip uses scissor fast path.
- [ ] Border radius does not automatically clip children.
- [ ] Stable complex clip can enter the mask cache.
- [ ] Simple shadow does not default to creating a blur layer.
- [ ] General blur/backdrop uses tight ROI.
- [ ] Multiple backdrops/materials can share capture/blur.
- [ ] Local color effects can fuse.
- [ ] Offscreen layer has an explicit LayerReason.
- [ ] Group opacity isolates only when semantically required.
- [ ] GPU canonical alpha is premultiplied.
- [ ] HDR/wide-gamut intermediate is not wrongly degraded.
