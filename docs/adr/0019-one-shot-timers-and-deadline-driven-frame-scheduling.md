# ADR 0019 — One-shot timers and deadline-driven frame scheduling (`WaitUntil`, driver-owned timer store)

- Status: Accepted
- Date: 2026-09-07

## Context

Tier 4 introduced the first control that must act *later* without input: the `Toast`,
which auto-dismisses after a `duration`. Two existing mechanisms could carry a delayed
effect, and they differ sharply on the AGENTS §7 hot-path / §60 "dev features must not tax
release" axes:

- **Ride the animation clock.** The `Sheet` slide already drives a per-frame
  `AnimationRegistry::tick`; a toast could poll a countdown on that same clock. But an
  animation clock only advances because the loop is *running a frame every vsync* — a 4s
  toast on a 60Hz clock costs ~240 wasted frames of pure countdown, each waking the CPU to
  do nothing. That is exactly the "zero CPU when idle" contract [[viso-macos-pump-autoreleasepool]]
  guards.
- **Block until the deadline.** The runtime already declared, but did not yet *produce*,
  the pieces of a deadline-driven wait: `RedrawReason::TimerDue` (`crates/runtime/src/schedule.rs:17`,
  covered by `decide()` tests) and `ControlFlow::WaitUntil(Instant)` (`crates/platform/src/control.rs:32`,
  which the three native backends already `matches!`-ed but collapsed into an unbounded
  `Wait`, discarding the deadline).

The governing criterion (best performance, least resource use, most reasonable design)
selects the second: a waiting toast should cost **zero frames** — the loop blocks until the
deadline instant, wakes once, fires, and idles again.

Frame-phase / scheduler semantics is an §68 ADR trigger, so this records the model.

## Decision

### 1. A one-shot timer store, owned by the driver (`crates/ui/src/timer.rs`)

`TimerRegistry` holds a `Vec<Timer>` (SoA-friendly, not a map — §29/§45); each `Timer` is
one-shot with a `deadline: Instant`, a `node: NodeId`, and an
`on_fire: Box<dyn FnOnce(&mut NodeStore)>` (`crates/ui/src/timer.rs:100`). The API is
`arm`/`arm_request` (deadline = `now + delay`), `earliest()` (`:208`, the smallest live
deadline), and `fire_due(&mut store, now)` (`:220`, swap-remove and fire every timer whose
deadline `<= now`; a dead node's timer is removed without firing).

`on_fire` receives **only `&mut NodeStore`**, the same discipline as `TranslateAnim::on_done`:
a fire cannot re-enter an `EventCx`, so it cannot arm another timer or start an animation
from inside a fire (no in-frame recursive-scheduling ambiguity). A control that needs an
`EventCx`-level effect on expiry (e.g. `Toast`'s `on_dismiss`) runs that on the *manual*
dismiss path, which has an `EventCx`; the store-only auto-fire just flips `hidden`.

Ownership sits on the driver (the facade `AppDriver`), not the scheduler: the time source
is the driver (`frame_now`, deterministic and headless-injectable), and the scheduler only
*queries* the earliest deadline. This keeps the scheduler stateless about timers and keeps
the clock in one place.

### 2. The scheduler emits `WaitUntil` only when idle (`crates/runtime/src/scheduler.rs:142`)

`FrameDriver` gained `next_timer_deadline(&self) -> Option<Instant>` (default `None`,
mirroring `wants_animation`). In `resolve_control_flow`, when the loop is otherwise idle
(no pending redraw reason, windows open) **and** the driver reports a deadline, the
scheduler returns `ControlFlow::WaitUntil(deadline)` (`:159`) — with **no** redraw reason
and **no** `Poll`. An armed timer therefore never self-schedules a frame; it blocks. An
active animation still takes the existing `Poll` path (it wakes every vsync anyway, and
`fire_due` rides along in that frame), so a timer that coexists with an animation costs no
extra frames.

Crucially, `PostFrameCleanup` self-continues **only** while an animation is live — a timer
does *not* keep the loop spinning. The wake comes from the backend honoring the deadline.
This is the whole point: the wait is a real block, not a poll loop.

### 3. Backends honor the deadline (`crates/platform/src/backend/`)

On `WaitUntil(deadline)` a native backend blocks with a timeout of `deadline - now` rather
than indefinitely: macOS uses it as the `nextEventMatchingMask` `untilDate`
(`macos.rs:198`); Windows and X11 use the equivalent bounded wait (`windows.rs:169`,
`x11.rs:168`). When the wait times out with no OS event, the backend synthesizes one beat
(a `RawEvent::Wakeup`, already handled by the scheduler as an `AsyncCompletion`) so the
driver runs one frame and calls `fire_due` at the head. No new `RawEvent` variant, no
scheduler structural change.

The headless backend (`headless.rs:91`) is purely deterministic and has no OS clock to
block against: `WaitUntil` there means the same as `Wait` — keep draining the queue. A
headless test crosses the deadline itself by advancing an injected clock
(`ManualClock`/`FixedStepClock`) and supplying the beat, exactly as `crates/viso/tests/toast_widget.rs`
and `crates/viso/tests/timer_loop.rs` do.

### 4. Facade seam (`crates/viso/src/lib.rs`)

`EventCx::request_timer(node, delay, on_fire)` is a deferred request, mirroring
`request_animation`: the router drains it after a handler returns, and the driver's
`FlushStateTransactions` phase arms each request against `cx.frame_now()` and then calls
`fire_due(&mut store, cx.frame_now())` at the frame head (`lib.rs:653`, `:656`). The driver
implements `next_timer_deadline` as `self.timers.earliest()` (`lib.rs:151`, `:778`). Timing
is always measured against `frame_now` (never a stray `Instant::now`), so a headless clock
governs the whole path.

## Consequences

- A waiting toast (or any one-shot timer) costs **zero frames** while pending: the loop
  blocks on the deadline and wakes exactly once. This is measurably better than a countdown
  on the animation clock (~240 idle frames for a 4s toast) and preserves the idle-CPU
  contract.
- Timers are one-shot and store-only on fire. Repeating timers and `cx.spawn` async tasks
  (the full §25 UI-task protocol: identity, cancellation, scoped ownership) are deliberately
  out of scope here and remain follow-ups.
- The scheduler stays timer-stateless: it queries one `Option<Instant>` and never owns a
  timer. The clock and the timer store both live on the driver, keeping determinism and
  headless injection in one place.
- Native `WaitUntil` timeout handling touches the platform pump. Headless does not compile
  the native backends, so the macOS deadline path must be verified on a real Metal run;
  Windows/X11 remain structurally symmetric with real-device verification deferred to CI,
  per the existing cross-platform convention.
