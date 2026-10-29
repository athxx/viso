# ADR 0044 — Device classes, footprint profiles and embedded targets

- Status: Accepted (2026-10-29)
- Date: 2026-10-17

## Context

Viso targets desktop, mobile and web. The retained tree, build-time AOT,
allocation-free steady frames, static backend selection and zero idle wakeups
already match embedded HMI requirements. Embedded HMI covers automotive,
industrial, appliance, medical and POS products. Nothing in the architecture
yet fixes a footprint contract, a backend for GPUs whose stable driver is
GLES, a production CPU rasterizer, or a compositor-less Linux backend.

Retrofitting MCU constraints after the core is written would mean rewriting
the arena, stores, text and AOT loading.

## Decision

- Device classes: Desktop, Mobile, Web, Embedded-GPU (Linux with GLES 3.0+ or
  Vulkan 1.1, at least 128 MB RAM), Embedded-CPU (Linux without a usable GPU,
  at least 32 MB RAM) and MCU.
  - Embedded-GPU is Tier-1 for 1.0.
  - Embedded-CPU is Tier-2 for 1.0.
  - MCU is not delivered in 1.0. The core keeps the MCU-ready invariants of
    Architecture §50A.6, checked in CI with a `thumbv7em-none-eabihf` smoke
    build.
- Footprint profiles (`desktop`, `mobile`, `web`, `embedded-gpu`,
  `embedded-cpu`) are fixed feature sets plus default budgets. Users choose a
  profile, never individual features. `cargo xtask check-profiles` validates
  each set.
- Backends:
  - `Gles3Backend` with GLSL ES 3.00 codegen. It is Tier-1 on Embedded-GPU and
    Tier-2 elsewhere, and declares its own capability profile. EGL and GLES
    are loaded at run time. Bind groups map to texture units plus sampler
    objects; user-program instances, with no storage buffers, are read from
    16 KB uniform-block chunks. Partial redraw uses buffer age,
    `EGL_KHR_partial_update` and swap-with-damage (Architecture §33.1).
  - `CpuRaster`, grown from `HeadlessRaster`. It is tile-based, damage-driven,
    supports RGB565/XRGB8888, uses SIMD with a bit-exact scalar fallback, and
    offloads to an optional 2D blitter. It consumes the same frame packet as
    the GPU backends.
  - Embedded backends are selected by profile feature rather than by
    `target_os`, and selection stays static.
- `backend/linux/kms`:
  - display through DRM/KMS atomic, with GBM/EGL or dumb buffers;
  - `FB_DAMAGE_CLIPS` and overlay/cursor planes;
  - input read directly from evdev, with libinput as an optional adapter;
  - one epoll loop with zero idle wakeups.
- Damage-driven present applies to every device class.
- `MemoryBudget` hands out byte budgets to every cache, GPU memory included.
  Exceeding a hard budget emits `BudgetExceeded`.
- Embedded fonts: no system font provider by default. Fonts are subset at
  build time from static strings plus a declared charset, with an optional
  baked A8 atlas. The rasterizer can be omitted when all text is baked.
- Development: an embedded profile runs on the desktop host with its exact
  feature set, display format and budgets. On the board, a remote Dev session
  receives `PatchBundle`s over TCP on the existing Ende protocol.
- CI records binary size, static RAM, idle RSS, peak heap, cold first-frame
  time and idle wakeups per profile, and compares them with an equivalent
  Makepad example.

## Consequences

- The budgets in Architecture §50A.5 are hypotheses until the first boards
  are measured.
- MCU support needs its own ADR. It opens once Embedded-CPU ships and the
  MCU-ready checks are green.
- `Viso_CLI.md` needs the embedded target, deploy and remote-session
  commands. `Viso_Rendering.md` and `Viso_Text_Font_Runtime.md` need the
  CpuRaster, damage and embedded-font sections.

## As built

- Profiles are feature sets of the facade that `cargo xtask check-profiles` holds to what each must not link; the facade has no feature named after a profile.
