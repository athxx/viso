# Viso Rendering Foundation — F0 → F4

Construction order is strict (Viso_Rendering.md §34): each layer's Done gate must be
green and its contract frozen before the next layer builds on it. The immediate-mode
boundary (`Primitive` enum + value types, `upload`/`submit` signatures, `paint.rs`,
`repaint_dirty`, facade prelude) stays frozen through F0–F4.

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
- [ ] `render/src/pool/coalescer.rs` (NEW): merge adjacent/nearby dirty instance slots
      into a few contiguous upload ranges (§9.3). Fixed-capacity scratch (arena- or
      high-water-backed, no steady-state heap alloc — §9.3); input is the pool's dirty
      slot set for the frame, output is a small list of `{offset, len}` ranges each
      emitted as one `backend.write_buffer(buf, offset, bytes)`. A gap smaller than a
      tunable threshold is bridged (one range re-uploading a few clean slots beats many
      tiny copies); a large gap splits into a new range.
- [ ] Wire the pool → coalescer → `write_buffer`: replace the per-family full
      `write_buffer(buf, 0, all_bytes)` with the coalesced ranges. A steady-state frame
      with no dirty slots issues zero `write_buffer` calls for that family.
- [ ] `render/tests/coalescer.rs` (NEW): scattered dirty slots within the gap threshold
      merge into one range; slots past the threshold split into separate ranges; a
      single dirty slot (the hover case) yields exactly one minimal range; zero dirty
      slots yield zero ranges. Fixed-capacity scratch does not allocate across frames.
- [ ] Gate green + golden byte-identical + steady-state bench: one hover-style dirty
      uploads exactly one coalesced range / touches one slot (proves §9.1 + §9.3).

### F4.5 — Order-safe batch planner (packed `BatchKey`, replaces segment merge)
- [ ] `render/src/batch/chunk.rs` (NEW): `RenderChunk` — `{ order range, bounds,
      primitive ranges, pipeline/material summary, clip chain, effect deps, revision }`
      (§9.6). A chunk is the unit of incremental rebuild: an unchanged chunk (its
      revision snapshot matches) contributes its cached batch structure untouched; a
      local change rebuilds only its chunk. Chunks partition the paint-order spine into
      contiguous ranges.
- [ ] `render/src/batch/planner.rs` (NEW): `BatchKey` — a packed integer (no strings,
      §16.2, §29) over `{ pipeline family/variant, render target, blend, clip mode,
      sample count, color-target class, resource table/texture set, depth/stencil class
      }` (§9.6). The planner walks paint order and forms the MAXIMUM contiguous run of
      compatible instances (equal `BatchKey`) — the goal is max contiguous compatible
      instances within correct paint order, NOT fewest draws (§9.6, §16.2). The reorder
      window widens only across spans explicitly marked opaque/reorder-safe; otherwise
      paint order (F3 §8.6) is preserved exactly. Replaces the adjacent-only
      `(kind, clip, target)` `segments.last_mut()` merge.
- [ ] Emit `DrawCommand`/`RenderPass` from batches: each batch → one `DrawCommand`
      pointing into the persistent pool at the batch's `instance_offset`; passes as
      today (offscreen-first, then surface). Batch scratch bump-allocates from the F4.1
      arena; `HashMap` only on the cold chunk/batch construction path (§9.5).
- [ ] `render/tests/batch_planner.rs` (NEW): interleaved primitives with equal
      `BatchKey` but split by an intervening incompatible one stay in paint order (not
      merged across the barrier unless reorder-safe); a run of same-key primitives
      collapses to one batch; `BatchKey` packs/unpacks losslessly; an unchanged chunk
      reuses its batch structure (no rebuild).
- [ ] Complete `FrameStats`/§61: `gpu_upload_bytes` (coalescer range sizes),
      `draw_calls`/`batches` (planner), `quad_instances`/`glyph_instances` (pool),
      `allocations_per_frame` (assert 0 steady state). Extend `inspect.rs`/§62:
      `BatchId` → pipeline/resources, `RenderChunkId` → ranges, upload-range visibility
      — cold path only.
- [ ] Gate green + golden byte-identical + steady-state bench: two warmed frames
      identical alloc (target 0 heap), `buffer_count` flat.

### Freeze
- [ ] FREEZE F4: `BatchKey` (bit layout + field set), `RenderChunk` shape, the
      instance-pool slot layout (`InstanceSlot { offset, len }` + `PrimitiveId`→slot
      table), and the upload-ring / coalescer contracts — before D0. D/C/E/M/A build on
      these. Machine-enforced in `render/tests/data_path_contract_frozen.rs` (one
      consolidated gate: `BatchKey` pack/unpack round-trip + field bit-widths;
      `RenderChunk` field set; `InstanceSlot` shape; ring/coalescer range invariants),
      alongside the per-module unit tests. Record the frozen contract inline here
      (mirroring the F3 freeze block) once the shapes settle.
