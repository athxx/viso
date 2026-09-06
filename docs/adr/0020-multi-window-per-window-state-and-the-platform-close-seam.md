# ADR 0020 — Multi-window: per-window retained state, deferred open/close seam, and the platform close seam

- Status: Accepted
- Date: 2026-09-07

## Context

Tier 4 closes with `Window`, the first control that is not a node. Tabs, NavigationStack,
Popup, Modal, Sheet, and Toast are all node controls an app builds into its `BuildCx`;
they enter one window's tree and share its store, layout, and frame loop. A window is the
opposite: a top-level OS surface that *hosts* a tree. Supporting more than one of them at
once forces decisions on three of the AGENTS §68 ADR triggers at once — frame phase
semantics (a frame now fans out over N windows), node ownership/identity (each window owns
its own node arena), and public application/component lifecycle (an app opens and closes
windows mid-session) — plus a platform-contract change (the OS seam must be able to close a
window on command, not only report that the user closed one).

The governing design criterion for this tier, restated by the user and authoritative here,
is: choose whichever option has the best performance, least resource usage, best effect,
and most reasonable/usable design — do not weigh implementation difficulty or code volume.
Under that criterion the questions were: how is per-window state stored and iterated; how
does a handler open or close a window when the new window's store does not exist yet and the
UI tier holds no platform seam; and how does exactly one teardown path serve both a
user-driven OS close and a programmatic close. The pre-existing infrastructure already
carried a `WindowId` on every `RawEvent` and already counted open windows in the scheduler
(`open_windows`, decremented on `WindowClosed`, exit-gated by `launched && open_windows == 0`
at `crates/runtime/src/scheduler.rs:143`), so the work was to converge the remaining seams,
not to thread window identity through every layer.

## Decision

### 1. Per-window retained state is a `Vec<WindowState>`, iterated — not a `HashMap`, keyed

The facade `AppDriver` owns one session-level `Application` and its `AppCx`, plus a flat
`windows: Vec<WindowState>` (`crates/viso/src/lib.rs:228`, field at `:242`). Each
`WindowState` (`crates/viso/src/lib.rs:260`) holds *all* per-window retained state — the
`WindowId`, the optional `GpuState`, the surface extent, the node `store`, the reactive
stores (`states`/`bindings`/computeds/effects/virtual-lists/text-edits), the per-window
`animations` and `timers`, the `root`, the routing scratch, and the text/shaping state.
A window's node arena is therefore its own: `NodeId` is a per-window index into that
window's store, so two windows legitimately share the value `NodeId { index: 0 }` for their
roots while naming different nodes in disjoint arenas. There is no global node id space.

`Vec` + linear scan is chosen over a `HashMap<WindowId, …>` deliberately, under AGENTS §29
/ §45. Top-level window counts are single-digit in practice, and the per-frame access
pattern is *iterate all windows* (run each phase over every window), not *look one window up
by key*. §45 names "lookup every … every frame" as the map-worthy case and the per-node/
per-frame key lookup as the anti-pattern; iterating a handful of windows is the former's
opposite. A flat `Vec` has no hashing, no bucket allocation, and contiguous layout — the
best performance and least resource use for this N and this access pattern. The only keyed
access is the rare event-routing lookup (`window_mut(id)` linear scan,
`crates/viso/src/lib.rs:392`), which for single-digit N beats a hash.

### 2. A frame fans out over windows; idle folds across all of them

`run_phase(phase, cx)` (`crates/viso/src/lib.rs:764`) iterates `self.windows`, running the
phase against each window's own store, reactive stores, animations, timers, and GPU. The
scheduler's frame phases are unchanged; what changed is that the driver's response to each
phase is now per-window. The two idle predicates fold across all windows:
`wants_animation` (`crates/viso/src/lib.rs:1016`) is true if *any* window has live
animations, and `next_timer_deadline` (`:1026`) is the *earliest* deadline over all windows.
This preserves the ADR 0019 zero-CPU-when-idle contract across multiple windows: when every
window is idle the fold reports no animation and no deadline, and the loop blocks rather than
spins.

### 3. Opening and closing a window are deferred requests, serviced in `run_phase`

A handler cannot open an OS window at call time: the UI tier holds no platform seam, and the
new window's store does not yet exist. So `window(config).content(build).open(ev)`
(`crates/viso/src/window.rs`) records a *tracked* deferred open on the `EventCx` — the same
three-hop request seam animations and one-shot timers ride — and hands back a cheap
`WindowHandle` immediately, backed by a shared id slot (`Rc<Cell<Option<u32>>>`). The build
closure is deferred: it runs later, against the *new* window's fresh store, exactly as a
first window's `Application::build` does. The facade services the request in `run_phase`,
where it holds the `RuntimeCx`: it calls `cx.create_window(config)` for a real id, brings up
a `WindowState` (`WindowState::open`, `crates/viso/src/lib.rs:443`), runs the deferred build
against that store, marks its root dirty, and back-fills the id slot so the handle resolves.
`WindowHandle::close(ev)` symmetrically records a deferred close carrying the raw id.

### 4. One teardown path serves both OS-driven and programmatic close (platform close seam)

The platform `PlatformApp` trait gained `close_window(WindowId)`
(`crates/platform/src/lib.rs:73`), implemented by every backend and by the headless backend
(which front-queues a `RawEvent::WindowClosed` so a programmatic close is delivered on the
next pump, deterministically, exactly like a user-driven close). `RuntimeCx::close_window`
(`crates/runtime/src/context.rs:119`) exposes it to the facade. A deferred close drained in
`run_phase` therefore does *not* retain-drop the `WindowState` directly; it calls
`cx.close_window(id)`, which asks the platform to close the window, which routes through the
single teardown path: `RawEvent::WindowClosed` → the scheduler → the new
`FrameDriver::on_window_closed` hook. The scheduler invokes the hook *before* decrementing
`open_windows` (`crates/runtime/src/scheduler.rs:245`, default no-op at
`crates/runtime/src/driver.rs:50`), so the facade tears the window's state down before the
exit gate reads the count. The facade's `on_window_closed` (`crates/viso/src/lib.rs:999`)
does `windows.retain(…)`, dropping that window's entire `WindowState` — store, GPU surface,
timers, animations, effects — in one place. Both a user closing a window and a
`WindowHandle::close` converge here; there is no second teardown route to keep consistent.

## Consequences

- Per-window isolation is total and testable: two windows own disjoint node arenas, stores,
  geometry, semantics trees, and frame work. `crates/viso/tests/window_multi.rs` proves each
  dimension — independent goldens, per-window pointer routing (a `RawPointer` reaches only
  the window it names), per-window geometry (a `Resized` changes only the named window's
  extent), independent semantics, and the cross-window idle fold — and the allocation profile
  shows each window's steady frame is deterministic and reuses GPU resources per §17.4 / §47.
- The public surface stays a single point: `window()` / `WindowBuilder` / `WindowHandle` /
  `WindowConfig` in the prelude. A window is an application-level handle, not a node control,
  so it does not appear in the widget/control registry the node controls use.
- `NodeId` is per-window. Any code that assumed a process-global node id space is wrong under
  multiple windows; tooling and tests must qualify a `NodeId` by its window's store.
- The linear-scan `Vec<WindowState>` is right for single-digit window counts. If a workload
  ever holds many tens of top-level windows and per-event `window_mut` lookup shows up in a
  profile, revisiting the store (e.g. a small direct-index map keyed by the dense `WindowId`)
  would be an ADR-worthy change with benchmark evidence — not a silent swap.
- The platform close seam is a genuine contract addition: every current and future backend
  must implement `close_window`. The headless backend's front-queued `WindowClosed` keeps the
  programmatic-close path deterministic and identical to the OS path under test.
- Effect teardown on window close runs through the window's `EffectStore` as its `WindowState`
  is dropped; whole-window close is the first live caller of that cleanup path.
