# ADR 0054 — Copy control handles over a generational control table

- Status: Proposed
- Date: 2026-10-27

## Context

A control an application drives from code (a modal, a popup, a toast host, a navigation
stack, a data grid, a map) builds nodes plus a block of shared parts that its handlers work
on: state ids, node ids and a few cells. The application needs a way to reach those parts
from its own handlers. Before this decision, the control's ids were allocated inside `build`,
and `build` returns nothing to a builder chain. So the application created an
`Rc<RefCell<Option<H>>>` slot for each control, which `build` filled once. The handle itself
held `Rc`s and was `Clone` but not `Copy`. Each capturing closure had to clone the slot or the
handle, and code that read the slot before the build ran found `None`.

## Decision

- `viso-ui` gains `viso_ui::control`:
  - **`ControlId`**: a generational `(index, generation)`. `ControlId::DETACHED` names no
    control.
  - **`ControlTable`**: kept by the `StateStore`. It maps ids to the control's parts, held as
    `Weak<dyn Any>`.
  - **`ControlHandle`**: a trait (`from_id`, `id`).
  - **`control_handle!`**: declares a control's `Copy` newtype over a `ControlId`.
  - **`Controls`**: the trait every context that can see the table implements (`StateStore`,
    `BuildCx`, `EventCx`, `UpdateCx`, `EffectCx`, `dyn StateWriter`). Its `control::<P>(id)`
    returns the parts while they live and are a `P`.
- Minting and binding:
  - `BuildCx::handle::<H>()` (and `StateStore::handle`) reserves a slot and returns its
    handle. A reserved slot is kept until bound, however long the build takes.
  - The control's `build` calls `BuildCx::bind_control(id, owner, Rc<P>)`, which binds the
    parts weakly in the table. The owner node holds them strongly (a node column freed with
    the node), so they live exactly as long as the control's root.
  - A build without a state store (`BuildCx::new`, layout only) mints `DETACHED` handles,
    and binding a `DETACHED` id does nothing.
- Each control builder:
  - takes an optional `.handle(h)`;
  - has an inherent `build(cx) -> XHandle` that mints one when none was given, builds, and
    returns it.
  - An application that needs the handle in a closure declared before the control
    (including the control's own handlers) mints it first with `cx.handle()`.
- Handle methods take a context:
  - Writes take `&mut EventCx` or `&mut dyn StateWriter`, and act through state writes and
    deferred requests, as before.
  - Reads take `&(impl Controls + ?Sized)` and return `Option` or an empty value.
  - When the control is gone, the slot's parts no longer upgrade, so writes do nothing and
    reads return `None` or empty.
  - No method hands out a borrow of the parts beyond the call.
- Vacant slots are swept lazily. When the table would grow past twice its size at the last
  sweep, it returns slots whose parts are gone to a free list and bumps their generation, so
  ids still held stay stale.
- Each control names its own handle type (`ModalHandle`, `PopupHandle`, …). The `*HandleSlot`
  aliases are removed.

## Alternatives

- **One generic `Handle<T>`.** Rust's orphan rule forbids `impl viso_ui::Handle<Modal>` in
  `viso-widgets`, so the per-control methods could only be added through an extension trait
  per control. That is more surface to import, and it gives no type-safety gain over a
  newtype per control.
- **Allocating the ids at the end of the builder chain and returning a handle holding them.**
  This makes the handle `Copy` but still leaves no handle before the build, so a control's
  own handlers still cannot name it.
- **Keeping `Rc` parts in the handle.** A handle like that is not `Copy`. It keeps the parts
  alive after the control is gone, so a stale handle would keep acting on dead ids.

## Consequences

- Handles are `Copy` and can be captured by any number of closures without cloning.
  Forward references need no `Option`.
- A handle lookup is one bounds check, a generation compare, a `Weak` upgrade and a
  `TypeId` downcast. It runs on the cold event path only, never per frame.
- Freeing a node now also clears its handlers and key handlers, along with the parts it owns.
  So a control's closures and parts go when its root does, even if the store reuses the slot
  later.
