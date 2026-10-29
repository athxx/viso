# ADR 0056 — GPU device loss: the backend reports it, the window rebuilds on a new device

- Status: Accepted (2026-10-29)
- Date: 2026-10-27

## Context

A GPU device can go away under a running app: a driver update or restart, a hang the OS
recovers from, an external GPU unplugged, a browser dropping its WebGPU device. Every
object the device made goes with it. Before this decision the RHI had one hook,
`device_lost(surface)`, that only dropped a held drawable and unblocked the retire queue.
That handles a frame that was abandoned. It does not handle a device that is gone: D3D12
panicked on `DXGI_ERROR_DEVICE_REMOVED`, Vulkan panicked on `VK_ERROR_DEVICE_LOST`, GLES
panicked on `EGL_CONTEXT_LOST`, and no layer rebuilt anything.

## Decision

1. **Two RHI calls with separate meanings.** The old hook is renamed `abandon_frame`; its
   behaviour is unchanged. A new required `GpuBackend::is_lost()` reports a device that is
   gone. Each backend latches what its API reports:

   | Backend | Reported by |
   |---|---|
   | D3D12 | `DXGI_ERROR_DEVICE_REMOVED` / `RESET` from present |
   | Vulkan | `VK_ERROR_DEVICE_LOST` from submit, acquire, present or a frame fence wait |
   | Metal | a completed command buffer whose error is `DeviceRemoved` |
   | WebGPU | the `device.lost` promise |
   | GLES | `EGL_CONTEXT_LOST` from swap or make-current |

   Once a backend is lost, `begin_frame` is `None` and encodes are dropped. Dropping the
   backend is safe. Its owner creates nothing more on it.
   `try_create_device() -> Option<Backend>` opens a replacement, or answers `None` while
   no device can be opened. In a browser it asks WebGPU for a new device asynchronously.

2. **The facade rebuilds per window, from what the window keeps.** The GPU state lives per
   window (the backend, the renderer and the text stack). When a window's backend reports
   a loss, the next frame starts by dropping all of that state without releasing the old
   handles. It then forgets every GPU object the window's tables hold:
   - asset textures and models;
   - what the renderer held of each 3D scene slot;
   - installed brush programs;
   - video textures.

   A new device then gets a new surface, renderer and text stack. On it, all text is
   shaped again and the whole tree is painted.

   The asset table now keeps the newest encoded bytes of each resource that has no
   origin. A raster or model is decoded again from those bytes, or fetched again from
   its file or URL. Until it is resident again, its nodes draw nothing at the size they
   drew. Each scene slot is marked as not filled, so its filler draws it again. The
   retained tree and its state are untouched.

3. **Retry with a backoff.** While no device opens, the window draws nothing. It retries
   0.25 s after the first try, doubling up to 4 s, through `next_timer_deadline`. No frames
   spin meanwhile.

4. **Tested by a simulated loss.** `CpuRaster::lose_device` loses the headless device.
   `crates/viso/tests/device_loss.rs` loses it after each frame of a window that shows text
   and a photo. It checks that the window draws on a new device, byte for byte as it would
   have without the loss. D3D12 adds `D3D12Backend::remove_device`, which triggers a real
   removal on hardware.

## Consequences

- The RHI grows by one query (`is_lost`) and one constructor (`try_create_device`).
  `abandon_frame` keeps the old hook's contract under a name that says what it does.
- Encoded bytes given at run time stay alive for as long as their resource is in the
  table. `Static` bytes cost nothing; for `Shared` bytes this is usually one more
  reference to what the app holds already. Resources fetched from a file or URL keep
  nothing and are fetched again.
- A loss first noticed while a resource is being created (rather than at submit or
  present) can still fail in the backends' create paths. Making those paths tolerate a
  lost device is follow-up work. In the paths covered here, the next frame recovers.
