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
- [ ] `gpu/src/retire.rs` (NEW): `RetireQueue` + in-flight `Epoch`/`Fence` — a destroyed
      slot is parked with the epoch it was retired in and reclaimed (slot freed for reuse,
      generation bumped) only once the GPU has finished that epoch. `begin_frame`/`present`
      advance the epoch and signal completion. (Fence/in-flight/retire behavior informed by
      the makepad Metal semantics extraction — behavior only.)
- [ ] `gpu/src/backend.rs`: extend `GpuBackend` with
      `destroy_buffer/texture/sampler/pipeline/bind_group`, an epoch/fence concept on
      `begin_frame`/`present`, and drive the `RetireQueue`. `create_*`/`write_*`/`encode`
      signatures unchanged.
- [ ] `gpu/src/headless.rs` + `metal.rs`: implement `destroy_*` → retire; on epoch
      completion, reclaim into the `SlotMap` free-list (generation bumped so old handles go
      stale). Metal: honor GPU completion via command-buffer completion / fence.
- [ ] `render/src/renderer.rs`: buffer growth sites (799/817/839/862/871) `destroy_*` the
      old buffer (→ retire) before creating the larger one, instead of leaking the old slot.
- [ ] **Extend** `crates/render/benches/renderer_steady_state.rs` (never weakened):
      `buffer_count()` stays flat across warmed frames **and** the retire queue drains to
      empty (no unbounded growth of parked slots).
- [ ] `gpu/tests/generation.rs`: destroyed slot is reused only after its epoch completes;
      pre-fence reuse is impossible; buffer growth reclaims the old slot without premature
      free.
- [ ] Gate green (Metal + headless) + golden/extended-bench.

### F1.4 — `device_lost()` + surface lifecycle (acquire/present/resize/DPI/out-of-date)
- [ ] `gpu/src/backend.rs`: add a `device_lost()` recovery hook (§6.4) and the
      acquire/present/resize/DPI/out-of-date surface paths.
- [ ] `gpu/src/metal.rs`: handle nextDrawable failure / surface out-of-date / resize / DPI
      change → rebuild surface-dependent resources; `headless.rs`: deterministic no-op /
      resize path. (Device-loss recovery behavior informed by the makepad extraction.)
- [ ] `gpu/tests/`: device-loss recovery rebuilds resources; resize/DPI path is correct.
- [ ] Gate green (Metal + headless) + golden/bench.

### Freeze
- [ ] FREEZE F1: the `GpuBackend` trait surface (incl. `destroy_*`/fence/epoch/`device_lost`)
      + the generational `{index, generation}` handle scheme. F2 and F4 bind to these.

## F2 — Build-time compile → PipelineManifest + typed `GpuPod` ABI (`shader`, `macros`)
(expanded when F1 is frozen)

## F3 — Retained scene under frozen immediate-mode API (`render`)
(expanded when F2 is frozen)

## F4 — Persistent data path: instance pool / upload ring / coalescer / arena / batch / chunk (`render`)
(expanded when F3 is frozen)
