# ADR 0037 — Mounting a view's handlers on the behavior VM

- Status: Proposed
- Date: 2026-09-28

## Context

Handlers (`on click { count += 1; }`) type-check and lower to behavior chunks
(ADR 0035), but no target ran them: the macros, the hot reload commit and the
release package each mounted a static tree only. The three targets must run a
handler the same way, a pure Rust UI must not link a VM, and `viso-ui` must not
depend on `viso-behavior`.

## Decision

- A new crate `viso-view` joins the two runtimes: `ViewHost` owns one component
  instance of a verified module, mirrors component state slots into UI state
  cells, and runs a handler as one transaction (cells copied in, dirty slots
  written back through the deferred state flush). `attach`/`attach_node` install
  one pointer and one key handler per node from its `(EventRoute, handler index)`
  routes. Its edges are `viso-ui`, `viso-behavior` and `viso-ende`;
  `viso-behavior` gains a `viso-ende` edge for the module wire form.
- `viso-dsl::view_behavior` computes one table per compiled view — the module,
  its bytes, the state slots and each node's routes, keyed by the Binding IR's
  pre-order node key — and every target mounts from it:
  - `component!`/`view!` embed the module bytes; `__embedded` decodes them once
    per thread and a fresh host per mount;
  - the hot reload commit reloads the live host (state kept by name) or creates
    one, then reattaches every node's handlers or clears them;
  - `ViewPackage` carries the UI package, the module bytes, each state's slot
    and the handler rows; a view without handlers carries no module and mounts
    no host.
- A handler that cannot mount is `E3711`: on a `ui!` fragment, for an event the
  runtime does not deliver yet, or with a body the lowering does not represent.
  A hot reload with `E3711` keeps the last-good handlers.
- A runtime fault aborts that call, writes nothing back and is kept on the host.

## Consequences

- `Rc<RefCell<ViewHost>>` is shared by a view's node handlers; a node handler is
  taken out of the store before it runs and writes are deferred, so the borrow is
  never re-entered, and a re-entrant dispatch is skipped rather than panicking.
- Capture-phase routing, component event delivery to a parent and native
  capability grants for view handlers are not yet provided.
