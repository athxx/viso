# ADR 0024 — File tree flattened-visible-row model and set_item_count-driven structural reconcile

- Status: Accepted
- Date: 2026-09-08

## Context

The `FileTree` control (`crates/widgets/src/controls/file_tree/`, the second control
that is a directory per §5) is a Tier 5 tree-shaped file browser (todo.md:794):
expand/collapse of directory nodes, indentation by depth, single/multi selection,
and keyboard navigation. The spec (§12.4) requires that a large directory tree eat
the Tier 3 `VirtualList` — a 100k-file tree must not mount 100k nodes — and that
expand/collapse be a *structural* change, with stable per-node keys (path identity)
that preserve the expanded state across reorderings, plus WAI-ARIA `role=tree` /
`role=treeitem` semantics with `aria-expanded`.

Three of its requirements touch §68 trigger areas and do not fit the existing
patterns as-is, so they are recorded here rather than left implicit:

**1. A tree edited by discrete actions, virtualized over its visible rows.** A file
tree is a *forest* of owned `TreeNode`s; expand/collapse edits an `open` set of node
keys. Reactive scalar cells (`Bool`/`Int`/`Float`) cannot hold a tree or an open
set, and the tree is far too large to mount node-per-file (§12.4). The rows the user
actually sees are the pre-order flattening of the forest, descending only into open
directories — a `Vec<VisibleRow>` whose length changes every time a directory opens
or closes. That changing count is exactly what a keyed `VirtualList` virtualizes, so
the control must drive the list's item count from the flattened row model rather than
authoring a node per file.

**2. Where the row-count change is applied.** `EventCx` holds no stores (ADR 0023):
a handler can only record intent. The reflatten and the list-count update hold
`&mut NodeStore` and `&mut VirtualLists`, so — as with `Dock` — they belong in a
store-mutating reconcile step the control owns, scheduled in the frame's layout phase
before `virtual_list::reconcile`, not in the event handler.

**3. Per-row `aria-selected`.** WAI-ARIA `treeitem` carries both `aria-expanded`
(already modelled by `SemanticState.expanded` / `with_expanded`) and `aria-selected`.
`SemanticState` has no `selected` field, and reusing `checked` would conflate a
checkbox's tri-nothing/checked state with tree selection — a distinct concept.

The governing criterion is the standing one: best performance, least resource use,
best effect, most reasonable design, easiest to use — not least code.

## Decision

### 1. Flattened-visible-row model + set_item_count-driven structural reconcile (generalizing ADR 0023)

The `FileTree`'s source of truth is warm owned state on the handle: the owned forest
of `TreeNode`s, the `open` set of `NodeKey`s (path identity), the selection set, the
focus cursor, and the *flattened* `Vec<VisibleRow>` derived from the forest and the
open set by a pure pre-order `flatten`. `VisibleRow` is `Copy`-hot (`key`, `depth`,
`is_dir`, `expanded`); the label `String` stays cold on the `TreeNode`, looked up by
key, never on the traversed row (§8.4).

Expand/collapse is the handler-writes-intent / reconcile-mutates-store split from
ADR 0023, extended to a control whose *row count* is variable:

- The handler (`command.rs`, holding only `&mut EventCx`) pushes an `Intent`
  (`Toggle`/`Expand`/`Collapse` of a `NodeKey`) into a shared `Rc<RefCell<Vec<…>>>`.
  It touches no store.
- The reconcile step (`reconcile.rs`, holding `&mut NodeStore` + `&mut VirtualLists`,
  scheduled before `virtual_list::reconcile`) drains the intents, edits the `open`
  set, reflattens the forest into the shared visible-row cell, and calls
  `set_item_count(lists, viewport, rows.len())` — which the keyed virtual list's next
  `reconcile` diffs by row `NodeKey`, mounting entrants, recycling leavers, and reusing
  survivors' hosts. No list rebuild; survivors keep host/state/focus.
- An empty intent queue returns early: a frame in which nothing was toggled does no
  reflatten and no allocation (verified by `tests/file_tree_reconcile_alloc.rs`, and
  benched by `benches/file_tree.rs`). A real toggle reflattens and re-drives the count,
  a genuine structural change that allocates a small bounded amount (§8.1).

This generalizes ADR 0023's "a control may own a store-mutating reconcile step" from a
control whose *tree shape* changes (Dock) to one whose *virtualized row count* changes
(FileTree): the reconcile step reflattens warm state and drives the substrate's
structural diff through the already-published `set_item_count`, rather than authoring
or tearing down nodes itself. Stable path keys in the `open`/selection sets keep
expanded and selected state attached to the node, not its position, so collapsing a
parent and re-expanding it — or reordering the data — does not lose descendant state
(§21.8, [[viso-diverge-from-makepad]]: Makepad's `file_tree` keys by path-hash id and
is single-select with no keyboard/a11y; Viso takes the identity semantics and adds
multi-select, keyboard nav, and semantics).

### 2. Additive semantics: Role::Tree / Role::TreeItem and SemanticState.selected

`Role` gains `Tree` (`role=tree`, the container) and `TreeItem` (`role=treeitem`, each
visible row) — additive variants like the existing `Region`, matching the enum doc's
"grows as controls land". `SemanticState` gains `selected: Option<bool>` with a
`with_selected(bool)` builder (mirroring the existing `expanded` / `with_expanded`),
kept `Copy` with no heap. Selection is distinct from `checked`, so it is a new field,
not a reuse.

The row's `expanded`/`selected` are fields of the warm flattened model, not reactive
scalar cells, so the reconcile step writes them **directly** via
`NodeStore::set_semantic_state(row_node, state)` — not through a `SemanticProjector`
alloc/compute cell, which exists to drive semantics *from* reactive scalar cells. The
per-row semantic write is part of the same store-mutating reconcile step, so it is
already on the correct phase with the correct borrows.

## Consequences

- `set_item_count` and `set_semantic_state` are already-published APIs with no new
  concept, so they need no ADR of their own; this ADR records only the *pattern* of
  driving a virtualized control's structural diff from a reflattened warm model, and
  the additive semantic surface.
- No dependency direction, node identity, frame-phase, or RHI contract changes — the
  reconcile step reuses the existing layout-phase seam (`crates/viso/src/lib.rs`) that
  ADR 0023 already established for a control-owned reconcile before
  `virtual_list::reconcile`.
- A future control whose visible-row count is derived from mutable warm state
  (an outline view, a variable-height data grid) reuses this pattern rather than
  inventing its own.
