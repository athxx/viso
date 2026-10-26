# ADR 0052 — Gamepads polled per frame into the game input

- Status: Accepted
- Date: 2026-10-26

## Context

Games written in `.vs` read their input through `InputMap` actions and the move vector
(`frame.input`), fed so far by the keyboard. Every OS has a gamepad API, but they differ
in shape: GameController and XInput are polled, evdev and the Android activity push
events, the web's Gamepad API hands out snapshots and fires only connection events. The
game wants one thing from all of them: the pads' state at the start of each frame, in one
layout, with connections it can show.

## Decision

- `viso-platform` owns one gamepad model (`viso_platform::gamepad`): `GamepadId` (unique to a
  connection, never reused), `GamepadState` (17 buttons by position, two sticks with y up,
  two triggers in `0..=1`), `GamepadInfo`, `GamepadEvent { Connected, Changed, Disconnected }`
  and `Rumble`. A shared normaliser gives every backend the same rest snap for sticks
  (radius 0.1, no rescale, so the game's dead zone keeps its meaning), trigger buttons with
  hysteresis (0.25 / 0.15) and an epsilon below which nothing is reported.
- `PlatformApp` gains `poll_gamepads(&mut Vec<GamepadEvent>)` and `rumble_gamepad(id, rumble)`,
  both defaulting to nothing. The first poll starts the backend's support; a backend whose OS
  pushes connections wakes the loop for them.
- The runtime polls once at each frame head, and on a loop wakeup, only while the
  `FrameDriver` says `wants_gamepads()`, into a buffer it reuses, and hands each event to
  `on_gamepad`. Events carry only changes; steady state allocates nothing.
- The facade sets `env.input.gamepad_available` per window and hands `Changed` to the frame
  clients of the window holding input focus while no UI node takes the keys, `Disconnected`
  to all. A game session merges every pad into local player 0 (buttons OR-ed, each stick from
  the pad pushing it furthest) and feeds the scheduler's input latch.
- The other way, `kit.rumble(strong, weak, seconds)` is a Presentation command; the stage keeps
  the last one, the game's frame client hands it over (`FrameClient::take_rumble`) and the
  facade plays it on every connected pad. Rust code calls `RuntimeCx::rumble_gamepad`.
- Backends: GameController (macOS, iOS), XInput loaded at runtime (Windows; GameInput is not
  used, as it has no stable ABI outside the GDK), evdev read directly with inotify hotplug
  (Linux, no udev), the activity's `InputManager` listener and gamepad key/motion events
  (Android), the Gamepad API (web), and test-driven pads on the headless backend.

## Consequences

- A pad's state is read only at frame heads: a running game sees every frame; a loop with no
  frames (a paused game, an idle UI) is woken for connections only, except on Android whose
  input is pushed.
- Pads do not drive UI focus navigation; they reach only frame clients.
- One local player: per-pad players are a later addition over the same ids.
