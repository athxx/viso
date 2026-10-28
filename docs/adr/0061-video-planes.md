# ADR 0061 — Video on display planes

- Status: Accepted
- Date: 2026-10-29

## Context

A board without a compositor drives its display through KMS. Display controllers on such
boards usually have overlay planes that scan out and scale NV12 pictures as they lie in
memory. Until now a video was always composited: each decoded picture was converted into a
texture and the window redrew the video's box. On a CPU-drawn board that means a colour
conversion, a scaled blit and a page flip for every picture. On a GLES board it means a GPU
pass per picture. Either way the cost per second grows with the video's frame rate, even when
nothing else on screen changes.

A plane can only stand in for the composited picture when nothing has to blend with it. The
picture must be on top, rectangular and drawn plainly: no rotation, tint, mask, rounded clip or
offscreen effect. And only the display can say whether it can scale, crop and place a given
picture.

## Decision

1. **`Scanout` gains planes** (`viso-handle`). Each of these has a default that does nothing,
   so other scanouts are unchanged:
   - `planes()` — how many pictures the display can show at once over the frames;
   - `show_plane(plane, Option<&PlaneImage>)` — show a picture on a plane, or take the plane
     down;
   - `move_plane(plane, crop, at)` — show the same picture at another crop or place.

   A `PlaneImage` is a `PlanePicture` (NV12 planes at hand, or an `ExternalImage` such as a
   decoder's dma-buf), plus a crop in texels and a place in frame pixels. `show_plane` returns
   whether the display shows the picture. A picture it refuses leaves the plane down, and the
   frame draws the picture as before. A picture shows with the next frame presented. When no
   frame is presented, it shows on its own at the next refresh.
2. **The renderer says where an image would show if it were drawn plainly on top.**
   `Renderer::topmost_image(texture, viewport)` returns the image's crop and screen rect.
   It returns `None` when the image is turned, tinted, masked, drawn offscreen, or when
   anything drawn later intersects it. `Renderer::texture_rewritten(texture)` marks the draws
   that sample a texture rewritten in place as damaged. Before this, such a rewrite did not
   redraw under partial redraw.
3. **The window's videos choose the plane once per frame.** This runs on a scanout window
   after the GPU upload phase and before submit (`Videos::scan_out`). It is a facade step with
   no user hook. A video found on top reserves a plane, and its next picture goes to the plane
   instead of the texture. While it is on the plane the window draws no frames for it.

   Once the video is covered or moved somewhere the display cannot show it, the next picture
   goes into the texture again. A paused video is sought where it is, so that a picture comes.
   The plane stays up until that texture is drawn, so the screen never shows a stale or empty
   box.

   A place the display refused is remembered and not offered again until the video moves.
4. **The KMS scanout implements planes** with the CRTC's overlay planes that stack above the
   primary plane, on an unturned panel:
   - An NV12 plane takes a dma-buf through `PRIME_FD_TO_HANDLE`, keeping a small cache of
     imports. It takes NV12 planes at hand copied into a pool of three dumb buffers.
   - A plane that scans out only XRGB takes NV12 planes at hand converted into its dumb
     buffers.
   - Every new size, crop or place is tried first with a `TEST_ONLY` commit.
   - A picture stays held until the flip after the one that replaces it.
   - When a frame declines to present, the scanout commits the planes on their own. When there
     is nothing to commit, the next frame waits for the next refresh through
     `DRM_IOCTL_CRTC_QUEUE_SEQUENCE`, not a timer.

## Consequences

- A playing video that nothing covers costs one copy into a dumb buffer per picture, or none
  for a dma-buf. There is no UI frame, no conversion on the CPU path and no GPU pass.
- Covering the video, rounding its corners or turning it moves it back to the texture path at
  the next picture. Nothing in the app changes, and the switch shows no gap.
- Only KMS implements planes. Compositor platforms keep compositing, or use their own video
  layers through native views (ADR 0060).
- Captures and screenshots of the window's frames do not contain a video shown on a plane.
