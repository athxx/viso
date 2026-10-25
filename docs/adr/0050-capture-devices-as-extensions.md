# ADR 0050 — Capture devices as extensions

- Status: Accepted
- Date: 2026-10-25

## Context

Cameras, microphones and speakers are devices, not UI. Each one needs:

- a large, target-specific API: AVCaptureSession, Media Foundation capture,
  Camera2, the GStreamer device monitor, `getUserMedia`, and on the audio
  side CoreAudio, WASAPI, AAudio, PulseAudio / PipeWire and Web Audio;
- permissions, and on mobile a session or focus that the whole app shares;
- policy that differs from app to app: which camera to open, at what mode,
  echo cancellation, mixing, latency.

If `viso-media` captured cameras itself, every app would carry those
backends, and the framework would have to settle choices that belong to the
app. What the framework does own is the path a picture takes to the screen:
the platform memory it lies in, how the GPU samples it without a copy, and
the widget that shows it. An extension cannot rebuild that path without
reaching into the renderer.

## Decision

- The framework keeps the protocol and the picture path. It owns no capture
  device.
  - `viso-media` defines `LiveSource` (poll, frame, play, pause), the source
    of live pictures, and `CameraProvider` (cameras, open).
  - An extension installs a provider once with `provide_cameras`. The first
    install wins.
  - `VideoSource::Camera`, `cameras()` and the `CameraView` widget route
    through the installed provider. With no provider, opening a camera
    returns `Unsupported` and `cameras()` returns an empty list.
  - `VideoPlayer::live` plays any `LiveSource`, such as a screen or a call.
  - A live source plays through the same frame path as a video. It has no
    duration and no sound, cannot seek, and its position is always zero.
- `viso-media` exposes one hook per target. Each hook hands over pictures the
  way that target's own player does, so the GPU still samples them where
  they lie:

  | Target  | Hook |
  |---------|------|
  | Apple   | `Frame::pixel_buffer` (IOSurface-backed `CVPixelBuffer`) |
  | Android | `ImageReader` (`AImageReader` hardware buffers, with the turns needed to show the picture upright) |
  | Windows | `SharedCanvas` / `SharedPictures` (the decoding D3D11 device, shared texture slots and a fence) |
  | Linux and the BSDs | `gstreamer_live` (a source element and caps, through the appsink path) |
  | Web     | `MediaStreams` through `web_live` (the hidden `<video>`) |

- On Android the platform sets `ndk-context` (the VM and the activity), so
  extensions can reach Java. Activity results reach extension Java through
  `VisoActivity`. An extension carries its own Java as a dex, which it loads
  under the activity's class loader.
- `viso-ext/viso-camera` implements cameras on every target behind
  `install()`. The camera example lives in that crate, so the framework's
  workspace builds without any extension.
- Audio is also an extension (`viso-ext/viso-audio`). That extension owns
  device listing, output and input streams on any device, capture and its
  permission, echo cancellation, sound playback, recording and the mobile
  audio session. The framework keeps two things of its own:
  - a video's sound, which stays with the platform player (ADR 0049);
  - `viso_platform::audio::AudioOutput`, the default output device that the
    DSL's game audio systems render to. It is output-only, has no device
    choice, and asks for no permission.

  Whether the game path should also go through a provider is open.

## Consequences

- An app that needs no camera or microphone links none of their backends
  and asks for none of their permissions.
- Capture policy lives in replaceable crates. An app can bring its own
  provider, for example one that uses a different camera API.
- Each hook is a public contract of `viso-media`. Changing what a hook hands
  over now affects extensions as well as the framework's own players.
- Without an extension, `CameraView` shows nothing and reports `Unsupported`.
  The extension's `install()` has to run before the first camera is opened.
