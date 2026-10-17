# Testing

## `viso test`

```bash
viso test                  # every domain: unit, ui, game, web
viso test unit             # cargo test
viso test ui               # every scenario under tests/ui/, headless
viso test ui clicks        # one scenario: tests/ui/clicks.tape
viso test game             # every tape under tests/game/
```

Each case prints one line and the run ends with a count:

```text
test ui clicks ... ok
test result: ok. 1 passed; 0 failed; 0 skipped
```

A domain with nothing to run here — no `tests/game/`, say — is reported as skipped
when you run them all, and is an error when you name it.

Options for every domain:

```text
--filter <PATTERN>   only the cases whose name contains PATTERN
--exact              --filter names a case exactly
--fail-fast          stop at the first failed case
--nocapture          show what every case writes, not only a failed one's
--jobs <N>           run at most N cases at once
```

`viso test unit` runs `cargo test` for the project's Rust code and reports each test
the harness ran as a case.

## UI scenarios

A UI scenario is a text file under `tests/ui/`, one step a line, `#` for comments.
It drives the app the way a user does — input goes through the platform, the same
route a real click takes — and checks what the app shows through its accessibility
tree:

```text
# The counter counts each click.
expect "0 clicks"
click button "Click me"
expect "1 click"
click "Click me"
expect "2 clicks"
```

The steps:

```text
surface 390x844@3            optional header: the window size and scale
settle 30                    optional header: frames to wait for the app to settle (default 120)
click <node>                 move the pointer to the node's center, press, release
hover <node>
type "hello"                 commit text to the focused node
key Primary+Shift+Z          Primary is Command on Apple platforms, Control elsewhere
scroll <node> 0 240          scroll over the node, in logical pixels
wait 30                      let 30 frames run
expect "Saved"               a node with this label is shown
expect no "Error"            none is
expect checkbox "Agree" checked
expect slider "Volume" value 0.5
expect snapshot "home"       take a snapshot and hold it to its golden
```

A node is its accessible label, `"Save"`, optionally with its role,
`button "Save"`, or an inspector selector in parentheses, `(Toolbar > #save)`. The
first one shown, in tree order, is used. States: `focused`, `checked`,
`unchecked`, `selected`, `expanded`, `collapsed`, `value <number>`.

Every step waits until the app has settled — no animation running, no text waiting
to commit — or the settle frames have passed. The first failed step ends the
scenario and is reported at its line, with the labels the window was showing:

```text
error[TEST_EXPECTATION]: no "3 clicks" is shown; the window shows "Viso", "Viso", "Minimize", "Maximize", "Close", "1 click", "Click me"
 --> tests/ui/wrong.tape:3:1
  |
3 | expect "3 clicks"
  | ^^^^^^^^^^^^^^^^^

test ui wrong ... FAILED
test result: FAILED. 0 passed; 1 failed; 0 skipped
```

Scenarios run headless by default: each in its own process and directory, in
parallel, with software rendering, so they run the same on every machine and in CI.

**On a device.** The same scenarios run on a simulator, an emulator or a browser:

```bash
viso test ui --target ios --device iphone
viso test ui --target android --device pixel
viso test ui --target web-gpu        # in headless Chrome, Chromium or Edge
```

Each case launches the app fresh through a `viso run` session and plays the
scenario inside it. Snapshot steps are taken headless only and are skipped on
devices. For the web, set `VISO_CHROME` to choose the browser; without one the cases
are skipped, not failed.

## Snapshots

A snapshot is the window's frame as a PNG, and a text form of its node tree, layout,
paint and batches. `expect snapshot "<name>"` in a scenario takes one; a scenario
with no snapshot step but a golden of its own name takes one at its end.

Goldens live in `tests/snapshots/`:

```text
tests/snapshots/home.headless.png
tests/snapshots/home.headless.json    size, surface, tree hash, tolerance
```

```text
$ viso snapshot capture
  snapshot   tests/snapshots/home.headless.png   7.73 KiB
  manifest   tests/snapshots/home.headless.json  337 B
1 snapshot captured; 0 unchanged
```

```bash
viso snapshot capture              # write every snapshot as its golden
viso snapshot capture home         # only those tests/ui/home.tape takes
viso snapshot compare              # hold every snapshot to its golden
viso snapshot update               # rewrite only the goldens that are missing or changed
viso snapshot update --filter settings
viso test ui --update-snapshots    # run the tests, rewriting changed goldens
```

A golden never changes unless you ask. Comparison is perceptual: pixels are
compared after compositing on white, and a golden's `tolerance` in its `.json` says
how far a pixel may drift (`threshold`, default `0.05`) and how many may
(`pixels`, default `0`). When a snapshot departs, `target/viso/snapshots/` gets the
new image, its tree, and a `.diff.png` that shows the golden in light gray, pixels
over the threshold in red, smaller differences in yellow, and areas only one side
has in magenta.

**A component alone.** Name a component instead of a scenario to snapshot it by
itself, its inputs at their defaults:

```bash
viso snapshot capture ui::Card
```

## Game tapes

For game systems, a scenario under `tests/game/` is an input tape plus expectations
checked after a tick runs:

```text
120: expect Player.y == 0.0
```

```bash
viso test game                          # every tape
viso test game jump --frames 600        # one, for 600 fixed ticks
viso test game jump --seed 1234         # over the tape's random seed
viso test game --tape runs/jump.tape    # replay a recorded tape
```

Each frame runs one fixed tick, so a run is deterministic: the same build and tape
give the same snapshot hash, which the result reports. `--json` adds a `trace` event
each tick with the values of the `@probe` states. Record a tape from the running
game with [`viso game record`](dev-loop.md#viso-game-the-running-game).
