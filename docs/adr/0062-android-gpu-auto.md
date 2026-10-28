# ADR 0062 — Android chooses its GPU API at run time

- Status: Accepted
- Date: 2026-10-29
- Amends: [ADR 0029](0029-gpu-backend-static-selection.md) (Android only)

## Context

ADR 0029 compiles one backend per target, and on Android that backend is Vulkan. Android
Vulkan drivers vary a lot more than desktop ones. Some devices have no usable Vulkan at all:
old or low-end GPUs, devices whose driver misreports what it supports, and the emulator's
SwiftShader image, where creating the instance fails. A Vulkan-only app panics on all of
these. GLES 3 is present on every Android device the framework targets. `Gles3Backend` already
exists, but choosing it meant a separate build and so a separate store package.

Other frameworks:

- Makepad defaults to GL on Android. Its opt-in Vulkan build falls back to GL when the Vulkan
  device does not open, and it branches on the API inside each pass.
- Flutter's Impeller ships both APIs. It picks Vulkan when the driver's version, its
  extensions and a denylist allow it, and GLES otherwise. The app's manifest can override
  the choice.

On the reference app (aarch64, footprint profile; the `auto` row is this decision's):

| Build              | Binary    | gzip      | `.text`   |
|--------------------|-----------|-----------|-----------|
| CPU only           | 9,134,432 | 4,425,574 | 6,757,188 |
| Vulkan             | 9,614,296 | 4,552,001 | 6,817,876 |
| GLES               | 9,716,152 | 4,606,152 | 6,929,764 |
| Both (`auto`)      | 9,930,392 | 4,699,263 | 7,073,380 |

## Decision

1. **`[target.android] gpu` in `Viso.toml`** takes `auto` (the default), `vulkan` or `gles`.
   The CLI passes the value to the build as `VISO_GPU`, and `viso-gpu`'s build script turns it
   into `cfg(viso_gpu = "auto" | "vulkan" | "gles")`. With `vulkan` or `gles` only that backend
   is compiled, as before. The `gles` feature still makes GLES the backend, as on Linux.
2. **With `auto`, `viso_gpu::Backend` is `AndroidBackend`**, an enum of `VulkanBackend` and
   `Gles3Backend`.
   - The API is chosen once per process: by the first backend made, or by the first
     `viso_gpu::shader_lang()` question, whichever comes first.
   - The choice opens a Vulkan device the way the Vulkan backend does: a loader, an
     instance, and a device with a graphics queue and everything the backend needs. If any
     of these is missing, the app draws with GLES and prints the reason once.
   - The launch variable `VISO_GPU=vulkan` or `VISO_GPU=gles` makes the choice instead. It
     arrives as an intent extra (`viso run --env`), which the facade passes to
     `viso_gpu::ask_api`, or through the process environment.
   - A backend made again after a device loss uses the same API.
3. **Dispatch is one `match` per RHI call.** That means per resource call, per draw list or
   per frame, never per primitive. The frame path still runs on concrete types inside each
   arm (AGENTS §17.2). Programs keep their AOT blobs for both APIs. `__per_backend!` and
   `shader_lang()` pick GLSL or SPIR-V when the API is known.
4. **GLES imports pictures without copying them**, so a device that falls back to GLES does not
   pay a CPU conversion for every video frame:
   - **An `AHardwareBuffer`** becomes an EGL image (`EGL_ANDROID_get_native_client_buffer`)
     and is sampled through `samplerExternalOES`, with the driver's Y′CbCr conversion. This is
     the same buffer the Vulkan path imports.
   - **An NV12 dma-buf** (Linux GLES boards) becomes an `R8` image and a `GR88` image
     (`EGL_EXT_image_dma_buf_import`, named linear when the modifiers extension exists).
   - **NV12 planes in memory** are uploaded to `R8` and `RG8` textures.
   - **Conversion:** each of these is one triangle drawn into the backend's own sRGB texture,
     using the picture's region and quarter turns, or its matrix, range and chroma siting.
     The framebuffer encodes into sRGB, so the shader decodes what it reads first.
   - **Caching and holds:** images are kept per `ExternalHolds`, the latest 16 at most.
     Images whose buffer is gone are dropped. A conversion holds the buffer until a fence
     placed after it signals.

## Consequences / 代价

- An `auto` app runs on every Android device with GLES 3, and keeps Vulkan wherever Vulkan
  works. One package serves both.
- The `auto` binary carries both backends: 316 KB more than Vulkan alone, 147 KB gzipped
  (the table above). A pinned build does not pay it.
- On Android, ADR 0029's "one binary, one backend" no longer holds. The branch is per RHI
  call, not per primitive, and every other target is unchanged.
- No driver denylist yet. A device whose Vulkan opens but misdraws needs `gpu = "gles"`,
  or `VISO_GPU=gles` while testing, until adoption data justifies a list.
- GLES reads textures back (`read_texture`, a verification path), so tests can check both
  APIs.
