# ADR 0029 — GPU backend static selection per target

- Status: Accepted
- Date: 2026-09-26

## Context

`Viso_Architecture.md` §33.1 and its ADR summary (entry "ADR-007：GPU backend 静态选择")
state the rule: one RHI in source, one backend specialization per target build, no
per-primitive `dyn GpuBackend` dispatch (AGENTS §17.2), Tier-1 backends per §48.1 /
AGENTS §65.1. The code cited that summary entry as "ADR-007", which collides with the
file record [ADR 0007](0007-scroll-viewport-transform-and-routing.md) (scroll viewport).
The selection itself — which backend each target compiles, what the fallback is, and how
an alternative backend is opted into — was never recorded as a file ADR.

## Decision

`viso-gpu` exposes one `GpuBackend` trait for source-level unification and one concrete
type alias, `viso_gpu::Backend`, chosen by `cfg` at compile time. The facade stores that
concrete type by value, so every frame-path call is monomorphized; no trait object is
held on the frame path.

| Target                                   | `Backend`        | Module     |
|------------------------------------------|------------------|------------|
| macOS, iOS (`target_vendor = "apple"`)   | `MetalBackend`   | `metal`    |
| Windows                                  | `D3D12Backend`   | `d3d12`    |
| Linux, Android                           | `VulkanBackend`  | `vulkan`   |
| Web (`wasm32-unknown-unknown`)           | `WebGpuBackend`  | `webgpu`   |
| any other target                         | `HeadlessRaster` | `headless` |

- `HeadlessRaster` compiles on every target: it is the deterministic test backend
  (AGENTS §66) and the fallback where no native backend exists.
- The `vulkan` feature compiles the Vulkan module on a target whose `Backend` is
  something else (for cross-backend testing); it does not change the selected alias.
- Backend-specific dependencies are target-scoped in `crates/gpu/Cargo.toml`, so a
  target links only its own backend's bindings.
- A Tier-2 compatibility backend (GL/GLES, a reduced Web path) is added as another
  module plus a `cfg`/feature selection of the alias, never as runtime dispatch, and
  declares its reduced capabilities through `Caps`.

## Consequences / 代价

- No backend-choice branch or virtual call on the frame path; each backend can take
  native fast paths behind `Caps` without a lowest-common-denominator RHI.
- One binary targets one backend: switching backend at runtime (e.g. Vulkan → GL on a
  weak Android device) needs a separate build or a future Tier-2 decision.
- Every backend implements the whole trait, so maintenance cost scales with the number
  of backends; CI must build each target to keep them honest.
- Code cites this record as "ADR 0029"; the Architecture summary entry "ADR-007" is its
  summary form.
