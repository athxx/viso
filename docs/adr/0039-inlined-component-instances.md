# ADR 0039 — Inlining user-component instances into the mounting view

- Status: Proposed
- Date: 2026-09-28

## Context

A view node whose type is a component of the same file resolved to a native
leaf, and a `bind` to a component input had no write-back. Every target (the
macros, the hot reload commit, the release package) mounts one component with
one handler table and one state layout. The runtime has no notion of a child
component instance, and giving it one would add a second mount path and a
second instance lifecycle to every target.

## Decision

- A user-component node is inlined at compile time. `ir::lower_component_view`
  walks the mounted view and a `ComponentLibrary` of the file's components.
  It lowers each instance's view in the node's place and records a `UiInstance`
  for each one, in pre-order: component, parent instance, identity, region
  depth, input arguments and wired handlers. Every node, pending property,
  handler and region records its instance, so a lookup key is a
  `Site { instance, at }` rather than a bare range.
- `behavior::inline_instances` copies every function of the child that touches
  a state, an input or an event, directly or through a callee. Entry points
  are copied too when the instance sits inside a region. Each copy is
  rewritten for its instance:
  - state slots move to hidden states `identity.state` appended to the mounted
    layout;
  - an input read calls the caller's argument entry, else the default, else
    `None`;
  - `emit` builds the event record and calls each wired caller handler;
  - the enclosing regions' scope values are threaded through as extra
    parameters or captures.
  Functions that touch none of these are shared by every instance.
- A `bind value <=> x` to a component input lowers to an argument reading
  `x` plus a write-back handler on the paired `@bindable` event, which stores
  the event's first field through the lens.
- `ir::lower_view_bindings` substitutes each instance's sources. A read of a
  child state binds its hidden state. A read of an input binds whatever the
  caller's argument reads, resolved through the caller's own instance.
- Hidden states are ordinary mounted states, so hot reload keeps them by
  name and the release package carries them unchanged.
- The hidden states of an instance inside a region belong to each mount of
  the region content: one set per keyed item and per entry into an arm.
  - Their initializers join the handler table as entries the region runs
    with its scope values when it mounts the instance. `ComponentLayout`
    records them per instance as `RegionalStates`, and the compiler creates
    no global cell for them.
  - Each arm template lists the slots it keeps (`LocalTemplate`). A mount
    holds their values and one revision cell per slot.
  - A dependency or binding edge names either a shared cell or a kept slot
    (`CellRef`). A region resolves a kept slot through its scope.
  - Before any call in its scope, the host loads the kept values into the
    VM slots. A write to a kept slot is stored back into the mount and
    raises the slot's revision and the view's pulse cell, so the view's
    structure hook sees it.
  - The states follow the key, are dropped with the item or with an arm
    that is not preserved, and are released when hot reload rebuilds the
    regions.
- Anything that cannot be inlined is `E3711`:
  - a component that mounts itself;
  - `bind … using` to an input;
  - properties or handlers forwarded to a view that does not mount exactly one
    node;
  - a component from another file.

## Consequences

- An instance costs no runtime node, no extra handler dispatch and no
  per-instance host. The only per-instance cost is its copied functions.
  Code size grows with the number of instances of a component that touches
  per-instance data. This is expected to beat a runtime instance model, but
  that is a hypothesis: no release measurement has been taken.
- An identity is positional: `Type#n` among same-type siblings unless the node
  is named. Inserting a same-type sibling before an unnamed instance moves its
  state on reload. Naming the node pins the identity.
- A caller's handler runs synchronously at the `emit`, inside the child's
  transaction.
- A regional instance's state starts over on each hot reload, because the
  regions are rebuilt. It is not kept by name.
- Instances in `ui!` fragments and components from other files remain open.
