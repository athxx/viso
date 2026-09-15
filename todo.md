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
- [ ] `gpu/src/resource.rs`: remove `PipelineDesc.shader_source: &'static str`;
      `PipelineDesc` carries the manifest artifact reference (family/variant + the
      `&'static str` MSL borrowed from the manifest entry) instead of a raw per-call string.
- [ ] `render/src/renderer.rs` (l.260-327): the four `create_pipeline` sites consume
      `standard_manifest()` entries (family lookup) rather than `QUAD_MSL()`/`quad_schema()`
      etc. directly; still pass `&QuadInstance::LAYOUT` for the §36.1 cross-check.
- [ ] `gpu/src/metal.rs` (create_pipeline, l.382-440): `newLibraryWithSource` compiles the
      manifest artifact's MSL — but this now runs only at device-init prewarm, never on the
      first-Button draw path. Keep `layout.validate_against(&desc.instance_schema)?`.
- [ ] `gpu/src/headless.rs`: unchanged builtin-tag path (never compiles MSL).
- [ ] Update `PipelineDesc` test sites (`headless_quad.rs` ~l.96-106 with `shader_source:""`).
- [ ] Gate green (Metal + headless) + golden/bench byte-identical.

### F2.5 — Zero-copy typed upload (§7.4) + dev-mode manifest source wiring
- [ ] Confirm/enforce the §7.4 path for the four instance stores: `&[GpuPod]` → typed byte
      view (`bytemuck`-free `unsafe` cast behind the `GpuPod` `Copy`+no-padding guarantee,
      one `SAFETY:` block) → `write_buffer` mapped range. Audit `renderer.rs` upload sites
      for any `Vec<Instance>→Vec<f32>→Vec<u8>` chain and remove it if present.
- [ ] Wire the existing `ShaderPipeline`/`CompiledShader` (`shader/src/reload.rs`) as the
      **dev-mode** manifest source (Viso_Hot_Reload §19-21: shadow-compile candidate IR,
      keep last-good on failure, swap at a safe frame boundary = F1 RetireQueue). Release
      uses the compile-time manifest; dev uses the reload holder. No release hot-path tax
      (§60 / Hot_Reload §1: no PatchBundle engine / dev transport in release).
- [ ] `render/tests/` (NEW): **release-no-runtime-compile** — a first-Button paint through
      the Metal path triggers zero `newLibraryWithSource` (instrument the backend with a
      compile counter; assert it is 0 after device-init prewarm across the first frame).
- [ ] Gate green + `metal_glyph.rs` green + §36.1 cross-check runs against manifest reflection.

### Freeze
- [ ] FREEZE F2: the `PipelineManifest` shape, `PipelineFamily`/`VariantKey`, and the
      `GpuPod` ABI of the four instance structs (`QuadInstance`/`ImageInstance`/
      `GlyphInstance`/`MeshVertex`). F3 (stores/instances) and F4 (instance pool/upload
      ring/batch `BatchKey`) bind to these.

## F3 — Retained scene under frozen immediate-mode API (`render`)
(expanded when F2 is frozen)

## F4 — Persistent data path: instance pool / upload ring / coalescer / arena / batch / chunk (`render`)
(expanded when F3 is frozen)
