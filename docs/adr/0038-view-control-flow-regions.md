# ADR 0038 — Mounting a view's control-flow regions

- Status: Proposed
- Date: 2026-09-28

## Context

`if`, `match` and keyed `for` type-check and lower, but every target rejected
them: the macros, the hot reload commit and the release package built a static
tree only. A region must mount, switch and reorder retained nodes (no rebuild
of the view), run the same under every target, and `viso-ui` must keep no
knowledge of the behavior VM.

## Decision

- The Binding IR keeps one pre-order `NodeKey` over every node, region content
  included; a region consumes no key. A target authors only the *static* nodes
  (outside every region and under nodes that author children) and names them by
  their pre-order among themselves (`StaticNodes`), which is also the release
  package's node index.
- `viso-dsl::view_regions` lowers every region to a `ViewRegions` template in
  `viso-view`: the static parent and its slot order, each arm's flattened node
  templates with their state edges and handler routes, and the cells the
  region's entries read. The template has one wire form, embedded by the macros
  and carried by `ViewPackage`.
- A region's decisions (arm choice, scrutinee, iterable, key) are handler-table
  entries of a new chunk kind, `RegionEntry`: pure, called with the enclosing
  regions' bindings, evaluated by `ViewHost::evaluate`, which keeps no write and
  no event. The verifier admits a handler-table chunk that is a `Handler` taking
  a payload or a `RegionEntry`; the host refuses to dispatch one as the other.
- `viso-ui` gains structure hooks: a view registers one hook with the union of
  its regions' cells; `run_structure_hooks` runs it after the frame's flush when
  one of them changed. The hook edits the tree in place (`insert_before`,
  `free_tree`, binding pruning) and re-evaluates only the regions whose cells
  changed.
- A state no UI cell holds (a string, a list) gets an integer revision cell the
  host raises on every committed write, so a region can depend on it.
- A `preserve` arm keeps its most recent instance detached and remounts it; a
  keyed `for` moves retained nodes by key; a repeated key faults and commits
  nothing for that list.
- A region at the view root, content under a `VirtualList` and a region in a
  `ui!` fragment are `E3711`; a repeated `preserve` identity is `E3301`.
- Hot reload takes the full rebuild path when the old or the new view has
  regions, keeping state by name.

## Consequences

- The structure hook and the view's node handlers share the view's
  `Rc<RefCell<ViewHost>>`. Both run on the cold path (after the flush, or from
  a dispatched event); neither runs inside the other, so the borrow is never
  re-entered.
- A hot reload of a view with regions loses focus and scroll positions.
- Region node property values that are not static edges are not evaluated;
  detached preserved nodes keep their bindings; `VirtualList` content from a
  view is not supported yet.
