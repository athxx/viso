# The dev loop

## `viso run`

```bash
viso run                      # on this machine
viso run ios                  # on an iOS simulator
viso run android              # on an Android emulator
viso run web-gpu              # in a browser
```

`viso run` builds a **dev artifact** — the app with the Viso dev runtime compiled
in — starts it, and keeps a dev session open until you press Ctrl-C. While it runs,
it watches the project and applies every edit you save:

| You edit | What happens |
|---|---|
| a `.vs` file | it is compiled on your machine and sent to the app as a typed patch; the app keeps its state |
| Rust source | the app is rebuilt in the background; the old one keeps running, then hands its state to the new one (a warm restart) |

An edit that does not compile never reaches the app: the running app keeps its last
good version, shows the error over its window, and `viso run` prints the
diagnostic. Fix it and save again.

Release and shipping builds contain none of this: hot reload exists only in dev
artifacts, so `viso run` always builds the `dev` profile and has no `--release`.
Use [`viso build --release` or `viso package`](packaging.md) to try a release
build.

### Options

```text
--device <ID>       the simulator or emulator profile (ios and android)
--browser <NAME>    chrome, safari, firefox, or none to only serve (web targets)
--open              open the page in a browser (web targets)
--no-hot-reload     keep the session, compile and report every edit, send nothing
--inspect           follow the app's input, focus and invalidation, as `viso inspect`
--profile-frame     draw frame times over the window and report every frame
--env KEY=VALUE     a variable the app is launched with; repeatable
--cwd <PATH>        the app's working directory (this machine only)
```

Arguments after `--` go to the app:

```bash
viso run -- --open demo.vs
```

An app reads `--env` variables with `viso::platform::launch_var("KEY")`, the same on
every target.
A value whose name looks secret (`TOKEN`, `SECRET`, `PASSWORD`, `KEY`, …) is never
printed, not even with `--verbose`.

### Stopping

The first Ctrl-C stops the build in progress and asks the app to quit; a second one
ends it at once. A simulator or emulator stays booted, so the next `viso run` does
not wait for it again.

Only one `viso run` runs per project at a time; a second one fails with
`ENV_LOCK_HELD` and names the one holding it.

## `viso inspect`: look inside the running app

With `viso run` running, open another terminal:

```bash
viso inspect                    # attach and follow what happens in the first window
viso inspect tree               # every node, window by window, with its box and source
viso inspect query '#save'      # the nodes a selector selects
viso inspect node '#save'       # all the app holds for them
viso inspect frame              # what the last frame drew: counters, batches, caches
viso inspect events             # input events, focus changes, nodes marked dirty and why
```

Selectors look like CSS: a type (`Button`, or one of your components), `#name`,
`.Style`, `[attribute]` or `[attribute=value]`, joined by a space (descendant) or
`>` (child), several separated by `,`:

```bash
viso inspect query 'Toolbar > Button'
viso inspect query '[role=check_box][checked]'
viso inspect node 12:1          # a node by its ID, index:generation
```

`viso inspect node` shows a node's resolved layout, style and content, its transform
chain, dirty classes, interaction states, the route an event takes to it, the
states it is bound to, and the paint primitives it produced. Each node names the
line of `.vs` that declares it.

To inspect without a session, run the app headless — optionally after a UI
scenario:

```bash
viso inspect tree --target headless
viso inspect query 'Text' --target headless --scenario clicks
```

`viso inspect --run` starts `viso run` for you and attaches once the app connects;
`--attach <id>` picks a session by the ID `viso run` reports.

## `viso profile`: measure frames

```bash
viso profile                                    # five seconds of the running app's frames
viso profile --frames 600                       # exactly 600 frames
viso profile ios --device iphone --seconds 10
viso profile headless --scenario scroll         # a headless run playing a scenario
viso profile --chrome trace.json                # also a Chrome trace, for Perfetto
```

For each frame it records the time of every phase — input, flush, text, layout,
style, paint, semantics, upload, submit — the GPU time, and what the frame did:
nodes, dirty nodes, measured and laid-out nodes, primitives, draw calls, batches,
uploads, shaped runs, glyph rasters. The summary gives p50, p95 and the maximum.
The trace is written under `target/viso/traces/` (or `--output`).

To count heap allocations per frame, install the counting allocator in the app:

```rust
#[global_allocator]
static ALLOCATOR: viso::alloc::Counting = viso::alloc::Counting::system();
```

`viso run --profile-frame` is the lightweight version: frame and GPU time drawn in
each window's corner, a line a second in the terminal.

A dev build is not optimized; for numbers you will compare, profile a headless run
or a release build.

## `viso game`: the running game

For projects with game systems:

```bash
viso game peek movement Player.score                 # read a state of the running game
viso game record movement --output runs/jump.tape    # restart the World, record 10 s of play
viso game record movement --output runs/long.tape --seconds 30
```

A recorded tape replays with `viso test game --tape runs/jump.tape`; add expectation
lines to it and it becomes a test scenario. See [Testing](testing.md).
