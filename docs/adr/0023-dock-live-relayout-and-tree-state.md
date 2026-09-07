# ADR 0023 — Dock live re-layout and tree-as-warm-state (a control that owns a store-mutating reconcile step)

- Status: Accepted
- Date: 2026-09-08

## Context

The `Dock` control (`crates/widgets/src/controls/dock/`, the repository's first
control that is a directory rather than a single file per §5) is an IDE-style
container of dockable, draggable panels: nested split/tabbed regions, a draggable
resize seam between splits, drag-to-redock, undock, and float into an overlay.

Two of its requirements do not fit the existing control patterns, and both touch
§68 trigger areas — layout sizing model and reactive semantics — so they are
recorded here rather than left implicit in the implementation.

**1. Live proportional seam resize.** The reference splitter (`splitter.rs`) sizes
its panes **once at build** (`paneA = Fixed(fraction·extent)`, `paneB = Fill`) and
pushes any live re-sizing to the application — there is no in-framework path for a
drag to change pane geometry after build, no minimum-pane floor, and the
fixed/fill split does not scale proportionally when the container resizes. A dock
seam must resize its panes **live**, proportionally, with a minimum-pane floor,
scaling with the container. That is a layout-sizing capability the splitter does
not have.

**2. A tree as the source of truth.** A dock's arrangement is a *tree* of splits
and tabbed regions, edited by discrete drag/command actions (redock, undock,
float). Reactive scalar cells (`Bool`/`Int`/`Float`) cannot hold a tree, and a
second synthetic-id `HashMap<Id, Node>` beside the generational node arena is
forbidden on any traversed path (§8.2, §29, §45). Panels must keep their identity —
and their built-once content subtree and reactive cells — as they move between
docked areas (§8.6, §21.8), which position-indexed identity (the way `Tabs` keys
its panels) cannot provide.

The governing criterion is the standing one: best performance, least resource use,
best effect, most reasonable design, easiest to use — not least code.

## Decision

### 1. Tree-as-warm-state + a store-mutating reconcile step (the reusable pattern)

The dock owns its `DockTree` — a plain, `Box`-linked enum of `DockNode`s
(`tree.rs`) — as **warm** state: built once, read by the build walk and the
reconcile step, never a per-frame path. This is deliberately *not* a bag of
reactive scalar cells (which cannot hold a tree) and *not* a virtual-DOM rebuild
target (§8.1, §59): ordinary resizes retarget retained nodes; only genuine
structural edits (redock/undock/float, a later slice) mark `STRUCTURE`.

The interaction split generalizes the one the virtualized list already uses:

- A seam's pointer/key **handler writes intent only** — it drives the seam's
  `fraction` cell through an `EventCx`, which by construction has no node
  geometry, no store, and no structural-mutation capability (§6.4). This is not a
  preference; the phase-scoped `EventCx` *cannot* resize a node, so a handler
  physically cannot do more than record intent.
- A **reconcile step holding `&mut NodeStore`** turns that committed intent into a
  live layout change: `reconcile_seams` (`reconcile.rs`) reads each seam's
  fraction, clamps it to the minimum-pane floor against the seam container's
  *resolved* extent, and rewrites both pane fill weights. It is allocation-free
  (it rewrites two `Length`s in place per seam), touches only the seams it is
  given, and runs when a fraction changed — not every frame.

This "control owns a store-mutating reconcile step, driven by handler-written
intent" is the reusable idiom this ADR records. It is the same
handler-writes-intent / reconcile-applies split the virtual list uses
(`set_absolute_rows_extent` in its reconcile), promoted from an internal list
mechanism to a documented pattern a control may adopt. The pitfall it guards
against: a later author reaching for `bind(size_cell, node, LAYOUT)` to make a
size "reactive" — a binding cannot rewrite a `Length`, so that silently does
nothing; live geometry must go through a `&mut NodeStore` reconcile step.

### 2. `NodeStore::set_flex_child_weight` — an in-place layout-input setter (no separate ADR)

The reconcile step rewrites a flex child's `Length::Fill { weight }` in place via
the new `NodeStore::set_flex_child_weight(child, axis, weight)`
(`component.rs`), marking `LAYOUT | PAINT`. Both panes are fill children, so the
parent redistributes leftover space by weight: pane A takes `fraction` of the
container, pane B the remainder — the split resizes proportionally and scales with
the container, and a minimum-pane floor is applied against the resolved extent.

This setter is **not itself an ADR-worthy change**: it is the same class of
in-place `LayoutInput` setter as the already-shipped `set_absolute_rows_extent`
(which the virtual list uses to rewrite a canvas's absolute extent during its own
reconcile). Same shape (rewrite one `Length` in place, mark the right dirty
classes), no new concept. Its allocation-free property is pinned by
`crates/ui/tests/flex_weight_relayout_alloc.rs` (a warmed weight-rewrite +
relayout allocates zero, `--test-threads=1`), and its live behavior by the dock's
`reconcile.rs` golden/bounds tests and the `dock/seam_reconcile_frame` microbench.

### 3. Tab-keyed panel identity

A panel is referenced only by a stable `Copy` `PanelKey(u32)`, never by position.
A panel's built-once node is found through a keyed
`Rc<RefCell<HashMap<PanelKey, NodeId>>>` on discrete drag/command actions (a cold
lookup, §45 — never a traversed path), so moving a panel between docked areas is a
tree edit plus a remount of the *existing* node, not a rebuild, and its reactive
cells survive the move (§8.6, §21.8).

### 4. Additive `Role::Region`

The dock container, and a floating panel's container, are named landmark regions.
`Role` gained a `Region` variant (`semantics.rs`), an additive extension in the
same spirit as the earlier `Dialog`/`Status` additions — `Group` is too weak for a
navigable landmark and `Navigation` carries the wrong semantics. Seams, panel
content roots, and transient drag chrome map to `Role::Group`; a tabbed region
with more than one panel reuses `TabList`/`Tab`.

## Consequences

- A control may now own a store-mutating reconcile step as a first-class pattern,
  not only the virtual list. Live geometry driven by discrete interaction has a
  documented home (handler writes intent → reconcile mutates store), and the
  "bind a size cell to LAYOUT" anti-pattern has an explicit warning.
- `set_flex_child_weight` is a general viso-ui capability (any flex child's weight
  can be rewritten live and allocation-free), not dock-specific; it is available
  to future controls without further review.
- The dependency direction is unchanged: the reconcile step lives in
  `viso-widgets`, holds a `&mut NodeStore` it is handed, and adds no edge; the new
  setter and `Role::Region` are additive viso-ui surface. `cargo xtask
  check-deps` stays clean.
- Not addressed here (explicitly deferred, see the module docs): dock tree
  persistence/serialization, lazy panel build, cross-window docking, and animated
  dock/float transitions.
