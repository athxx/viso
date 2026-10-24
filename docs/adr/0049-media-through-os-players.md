# ADR 0049 — Media through the OS players

- Status: Accepted
- Date: 2026-10-24

## Context

Video playback needs a demuxer, hardware decoders for H.264, HEVC, VP9 and
AV1, audio output, and sound kept in step with the picture. HDR and HLS or
DASH streaming come on top. Every target already ships all of this in its
media stack:

- AVFoundation on macOS and iOS;
- Media Foundation on Windows;
- GStreamer on Linux;
- MediaCodec and MediaPlayer on Android;
- the `<video>` element on the web.

Codecs are Tier D: Viso does not build its own (Architecture §3). The window
and input layer, `viso-platform`, must not pull in video. Each media stack is
a large, target-specific dependency that has nothing to do with windows.

## Decision

- A new leaf crate, `viso-media`, plays media through each target's own
  player. It depends on no framework crate. The facade depends on it.
- A `VideoPlayer` opens one of:
  - a file;
  - a URL, which the platform fetches itself (HLS included);
  - bytes in memory, which go to a temporary file on players that read
    files only.

  It opens paused. It is polled from the loop's thread for what changed:
  ready (with size, duration, frame rate, audio and seekability), ended,
  failed, or waiting for data. It is also polled for the picture to show now,
  which comes back only when that picture has changed. Play, pause, seek,
  rate, volume, mute and looping steer it. The platform decodes, plays the
  sound and keeps it in step. Viso only shows the picture.
- Pictures come from the platform as BGRA, already converted by its video
  processor. This takes care of YUV matrices, range, and HDR tone mapping.
  - A frame the CPU can read is copied into the video's resident texture.
  - A frame the GPU already holds is handed over as a native handle instead
    of being copied. The handle is an IOSurface, a shared D3D texture or a
    hardware buffer, named through `viso-handle`. A backend that can adopt
    it points the same texture at the platform's memory.
- In `viso-ui`, a video node is an asset node with a resource of its own,
  which the facade provides from the player rather than from a decoder. The
  asset machinery it gets for free:
  - fit, tint and the rounded mask;
  - poster and error stand-ins;
  - the `loaded` / `failed` notices;
  - the on-screen test.

  A per-node video table on top carries what the app asks for (playing,
  looping, muted, volume, rate, seeks) and what the player reports
  (position, duration, ended, waiting).
- Each new picture marks `PAINT` on the nodes showing it. Nothing is laid out
  again unless the picture's size changes. A playing video asks for frames
  while a node shows it on screen. Off screen it keeps playing, but no
  picture is read.

## Consequences

- No codec, demuxer or audio clock lives in Viso. What plays is what the
  platform plays: AVFoundation does not play WebM, for example, and that
  shows up as a failed open.
- Bytes on a file-only player cost a temporary file, removed when the player
  is dropped.
- Headless tests on macOS play real files through AVFoundation. Its run loop
  must turn on the main thread, so the player's integration test runs
  without the libtest harness.
