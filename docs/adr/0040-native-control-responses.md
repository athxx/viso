# ADR 0040 — Native control responses mounted from the handler table

- Status: Proposed
- Date: 2026-09-28

## Context

A `bind checked <=> on;` on a native `Toggle` had no write-back, and no
native control reported a change a view could handle. The toggle, slider,
tab strip and text field behavior lives in Rust widgets that own their state.
A view, however, mounts every node from one handler table and one state
layout, under the macros, the hot reload commit and the release package
alike. Mounting the Rust widgets inside a view would add a second state owner
per control and a second mount path per target.

A handler on an ancestor also ran twice per sample, on the capture leg and
again on the bubble leg. A pointer payload's position was in window
coordinates, not local to the node.

## Decision

- `viso-view` owns `Control { kind, value, min, max, step }`. `kind` is one of
  toggle, slider, select (tab strip or radio group) and text input. Each other
  field names an entry of the view's handler table: a pure chunk that
  evaluates the control's current value or range against the states and the
  item scope when a sample arrives. An absent entry reads the property's
  default.
- `attach` takes an optional control next to the routes. On the target and
  bubble legs, `Control::drive` computes the next value from the sample and
  delivers the event it pairs with, through the same routes as any handler:
  - `changed` for a toggle, a slider or a settled text edit;
  - `selected_changed` for a select;
  - `submitted` on Enter in a text field.
  Text editing is recorded as edit intents that the router applies to the
  node's buffer, which is created lazily and checked against its owner.
- The compiler lowers a native `bind` like a component-input `bind`:
  - a read entry for the property;
  - a write-back handler for the paired event. It writes field 0 of the
    payload to the lens, and it leads the node's handlers so that an author's
    handler for the same event sees the written state.
- The UI IR records which properties feed a control (`control_reads`).
  `ViewBehavior` resolves those reads to entries and carries one `Control`
  per node. Hot reload, the view package (`ViewPackage::controls` and the
  region `ItemTemplate::Node`) and the macro emitter all pass it to the same
  `attach`.
- A DSL handler runs once per sample per node: on the target and bubble legs,
  never on the capture leg. `EventCx` exposes the node, its rect, the phase
  and the child index the sample came through. A pointer payload position is
  local to the dispatching node.
- A native `bind … using` is `E3711` until converters are lowered.

## Consequences

- A control does not yet display a reactive value. Its node takes its value
  only through the response. Reactive property values reaching retained nodes
  belongs to the reactive graph.
- A `ui!` fragment has no state and no handler table, so it mounts no control
  response.
