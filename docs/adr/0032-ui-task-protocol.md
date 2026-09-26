# ADR 0032 — UI task protocol: loop-thread tasks, a cross-thread wake, node-scoped cancellation

- Status: Accepted
- Date: 2026-09-26

## Context

`Viso_Architecture.md` §13 and AGENTS §25 give Viso a UI task protocol: task identity,
wakeup, cancellation and scoped ownership. They do not give it a general executor, an
I/O reactor or an HTTP stack. `cx.spawn(async move { … })` is the default application
API. ADR 0031 made every asynchronous service call return a `Reply<T>` future, and it
deferred `cx.services()` until futures had somewhere to run.

Four facts shape the design:

- The UI tree is owned by the loop thread (§26). A UI task's continuation must touch
  state and nodes, so the future runs on the loop thread and can be `!Send`.
- A service reply is completed wherever the OS answers. That can be a panel callback
  on the main queue, a WinRT completion on a thread-pool thread, a D-Bus reader or a
  JNI call from the Java UI thread. The wake has to cross threads.
- The platform loops block when idle (zero CPU when idle). A wake that arrives while
  the loop sleeps must unblock it, and every backend blocks differently: an AppKit
  event wait, a Win32 message wait, `poll(2)` on X11/Wayland, `ALooper`, the UIKit run
  loop, and the browser event loop.
- A task spawned by a component must not outlive it. Continuations write into the
  arena, so a completed task for an unmounted node would write through a dead id.

## Decision

### 1. The platform exposes a `LoopWaker`

`viso_platform::LoopWaker` is a `Clone + Send + Sync` handle over one kick closure.
`PlatformApp::loop_waker()` returns it; the default is inert. A kick from any thread
eventually delivers one `RawEvent::Wakeup` on the loop thread. Kicks may coalesce.
Each backend kicks through the wake it already owns:

| Backend | Kick |
|---|---|
| Headless | an atomic flag, polled by `next_event` after pending redraws |
| macOS | an atomic flag on the pump queue, then `dispatch_async_f` to the main queue, which posts an application-defined event to end the event wait |
| Windows | an atomic flag on the pump queue, then `PostThreadMessageW` to the UI thread |
| Linux | `Wake::Tasks` on the existing wakeup channel and pipe |
| Android | `Msg::Wake` on the existing inbox, then `ALooper_wake` |
| iOS | `dispatch_async_f` to the main queue, which pushes `Wakeup` and drives |
| Web | a zero-delay `setTimeout` that pushes `Wakeup` and drives |

The kick carries no payload. Which tasks are ready is the runtime's business, not the
platform's.

### 2. The runtime owns task identity, the wake queue and the task set

`viso_runtime::task` provides:

- `TaskId`, a `Copy` process-unique id.
- `TaskSet<O, T>`, a set of `Pin<Box<dyn Future<Output = O>>>`. Each task carries a
  caller tag `T`, which the UI tier uses for the owning node. Its methods are `spawn`,
  `cancel`, `cancel_where`, `is_woken` and `poll`.
- One `std::task::Waker` per task, built once at spawn. Waking it pushes the task's
  id onto a shared ready list. Only the first wake after a drain kicks the
  `LoopWaker`; later wakes see the signalled flag and do not kick again. A spawn
  signals the same way, so a fresh task gets the frame that runs its first poll.
- A drain that clears the flag before it takes the list. The list's mutex is taken
  only when the flag was set, so a frame with no woken task takes no lock. The lock
  guards a `Vec<TaskId>` shared with foreign threads. It does not guard UI state.

`poll` polls only the woken tasks, in id order. A finished task leaves the set, and
its output is handed to the caller. Nothing is polled on a frame without wakes.

`FrameDriver::on_wakeup` is a new hook that runs on every `Wakeup`, with a live
`RuntimeCx`. The driver requests a redraw for the windows whose tasks woke, or whose
timers are due. The due check reads `FrameClock::peek`, so a deterministic clock that
steps per read is not stepped by a wake. The frame that follows polls them.

### 3. The UI tier spawns from `EventCx`, scoped to the handler's node

- `EventCx::spawn(fut)` runs a `Future<Output = ()>` and returns its `TaskId`.
- `EventCx::spawn_then(fut, then)` runs `fut` and then calls
  `then(&mut UpdateCx, output)` on the frame after it completes. This is how a task
  writes state: the continuation gets an `UpdateCx` only after the `.await`s are over,
  so no arena reference is ever held across an `.await`.
- `EventCx::cancel_task(id)` drops the task.

Spawns are queued on the context. The router hands them to the store under the node
whose handler ran. Freeing a subtree cancels every task the subtree owns, next to the
scoped-effect cancellation. Clearing the store or closing the window drops all of its
tasks. Cancellation means dropping the future. A dropped `Reply` releases its slot,
and the OS answer, if one still arrives, completes nothing.

The facade polls a window's woken tasks at the start of `FlushStateTransactions`. It
runs their continuations before the frame's state flush, so writes a continuation
makes flush in the same frame.

### 4. `cx.services()` reads a type-erased slot

`ui` cannot depend on `services` (§3.5), so the store holds the session's `Services`
as an `Rc<dyn Any>`. `EventCx` borrows it. The facade's `ServicesExt` trait, in the
prelude, gives `cx.services() -> &Services`. The facade creates the registry once per
session with `Services::system(app)`, where `app` is the executable's file stem. A
headless fallback gets `Services::unsupported()` instead, and headless tests can
inject a mock.

### 5. Runtime-bound futures go through adapters

A UI task is polled only by its `TaskSet` on the loop thread, and the set drives no
reactor. A future that needs one (a Tokio timer, a socket) must be spawned on that
runtime by an adapter, and the UI task awaits the adapter's join handle, which is
woken from the runtime's threads like any other waker. Futures that complete through a
waker alone need no adapter: `Reply`, channels, and `JoinHandle`s of foreign
executors. No adapter crate ships with this decision.

### 6. Deferred

- `spawn` and `services` on `UpdateCx`, `BuildCx` and a dedicated `TaskCx`.
- A detached (unscoped) spawn, and a cancellation token beyond `TaskId`.
- Adapter crates (`viso-tokio`, `viso-smol`).
- Wakes during a nested native modal loop: an AppKit modal session or a Win32
  move/size loop. The flag is checked on the modal pump where one exists. Otherwise
  the task waits until the modal loop returns.

## Consequences

- The idle path has no cost: no woken task means no lock and no poll, and the loop
  stays blocked.
- Every backend gains one wake variant or one main-queue hop. The frame loop itself is
  unchanged.
- A task's continuation always runs at a frame boundary, never inside an OS callback.
