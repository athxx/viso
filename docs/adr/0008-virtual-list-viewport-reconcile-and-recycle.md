# ADR 0008 — VirtualList: A Scroll Viewport, a Per-Frame Reconcile, and a Recycle Pool

- Status: Accepted
- Date: 2026-09-02

## Context

ADR 0007 landed **Scroll**: `bounds` is the unscrolled layout truth, `world` is
the derived on-screen rect, and `scroll_by` marks `TRANSFORM | HIT_TEST | PAINT`
only — never LAYOUT/MEASURE, and never bubbling. It closed by naming the next
Phase 4 slice: the virtualized list of §12.4, sitting on that viewport.

This slice lands it. §12.4 is a hard architectural requirement, not a widget nicety:

> A `List` with 100k logical items must not create 100k mounted nodes.

and it must support stable keys, recycling, a visible range + overscan, a
variable-height cache, and scroll-anchor preservation. Appendix C of the
architecture document gives the steady-state model: 100k items → viewport +
overscan → ~40 mounted item nodes → a scroll delta → the visible range changes by
~3 → recycle 3 old hosts + bind 3 new item records → only the changed instance
ranges upload. Makepad's `PortalList` is an **algorithm reference only** (§38.3):
Viso keeps the semantics (a height-indexed anchor, an overscan window, a recycle
pool) and reimplements them on the Node/State/Layout/Dirty contracts — a
data-oriented registry, a Fenwick height tree, and a per-frame reconcile, not a
`Widget`-owns-everything portal.

The tension this slice resolves: Scroll is **transform-driven** (children sit at
their real `bounds`; a wheel shifts only `world`), but virtualization needs a
*stable, huge* content extent for the scroll-range clamp **and** must *remount* a
few nodes on a row-boundary crossing (a structural/layout change). Those pull in
opposite directions — one wants zero layout on scroll, the other wants targeted
layout on a crossing. This touches three §68 ADR-trigger areas — the layout
sizing model (a new `LayoutInput` variant), node ownership/identity (a detach +
subtree-free recycle API), and reactive semantics (a driver-owned registry that
mounts/unmounts nodes outside `build`) — so the decisions are recorded together.

## Decision

### 1. A VirtualList *is* a Scroll viewport over a fixed-size canvas

No `LayoutInput::VirtualList` variant. Forking a new scroll-like input would
duplicate every `is_scroll` / `scroll_axis` / `scroll_range` / router site for no
semantic gain. Instead:

- The viewport is an ordinary `LayoutInput::Scroll { axis, size }` — so
  `scroll_by`, `scroll_range`, wheel routing, clip, and hit-test from ADR 0007
  work **unchanged**.
- Its single content child is a **canvas** sized `Length::Fixed(total_extent)` on
  the scroll axis (the height tree's running total) and `Fixed(0)` on the cross
  axis (the real cross extent is filled in from the viewport during layout). The
  canvas's own fixed extent *is* the spacer — there are no spacer nodes above or
  below the mounted window.

Because the canvas is `Fixed` on both axes, `layout_scroll` places it at its fixed
main extent and records `content = total_extent`, so `scroll_range == total −
viewport` is correct **without mounting all 100k items**.

### 2. `LayoutInput::AbsoluteRows { axis, size }` — absolutely-placed mounted rows

The canvas lays out with a new arm:

- `measure`: its natural size is its own `size` request. It does **not** sum its
  sparse children — the total is authoritative, set from the height tree.
- `layout`: each mounted child is placed at `main = row_offset(child)` and
  stretched to fill the canvas cross axis. `row_offset` is a new warm `Vec<f32>`
  column on `NodeStore` (sentinel = "not a positioned row"), read through a
  `LayoutTree::row_offset` hook — so the layout pass stays registry-free and
  allocation-free; it never touches `VirtualLists`.

Rows are absolutely placed, not a flex column of spacers + items. `LayoutInput`
stays `Copy`; `LayoutInput::size()` is extended to cover the new variant.

### 3. The load-bearing invariant: a remount marks the canvas `LAYOUT`, never `STRUCTURE`

`STRUCTURE` (and `SEMANTICS`) are `BUBBLING` dirty classes — they rise
unconditionally to the root, which would force a full-tree relayout on every
row-boundary crossing and blow the §12.4 budget. A `MEASURE`/`LAYOUT` mark,
however, stops rising at the first ancestor that is `Fixed` on both axes. The
canvas is exactly that. So the reconcile, when it recycles rows, marks the canvas
`MEASURE | LAYOUT | PAINT` and detaches/attaches hosts **directly on the arena
links** — never through the `build`-time child-append path that would mark
`STRUCTURE`. The invalidation stays contained to the canvas subtree; the next
`relayout_dirty` re-places only the mounted rows, not the tree. This manual
invalidation choice is the whole contract, verified against the `mark_dirty`
bubbling rule and the fixed-on-both-axes boundary check.

### 4. Data model — a Fenwick height tree and a driver-owned registry

- **`HeightTree`** — a Fenwick/BIT over `f32` (matching Viso geometry, not
  Makepad's f64), all operations O(log n) and allocation-free after construction:
  `prefix_sum`, `point_query`, `update` (true only on a real delta), `total`,
  `find_position(target) → (index, offset)` (a BIT binary search that inverts
  `prefix_sum`), `resize`, and `update_default_height` (re-applies the estimate to
  unmeasured rows only, past a drift threshold). Unmeasured rows carry the
  estimated row height; measured rows carry their real height.
- **`HeightCache`** — a running mean of measured heights, fallback = the estimate,
  used to seed newly mounted rows before they measure.
- **`VirtualListState`** — the per-list side struct: the height tree and cache, an
  index-relative anchor (`anchor_index`, `anchor_offset ≤ 0`), the current window
  (`range_start`, `range_end`), a `mounted: Vec<MountedItem>`, a `pool:
  Vec<NodeId>` of detached-but-valid hosts, the item count, overscan, the boxed
  item builder, the canvas id, `last_reconciled_scroll`, and a `dirty_data` flag.
  Its scratch `Vec`s are reused, never freed.
- **`VirtualLists`** — `Vec<Option<Box<VirtualListState>>>` indexed by the
  viewport's `NodeId::index()`. Reconcile lookup is one dense `Vec` index, no hash.
  It is **driver-owned** (a field on `AppDriver`, reset in `on_launch` beside
  `store.clear()`, like `StateStore`/`EffectStore`) — deliberately **not** a
  `NodeStore` column (heap-heavy warm state on a handful of nodes would pollute the
  dense hot/warm arrays, §8.4) and **not** a per-node HashMap (§7.1 / §45).
- **`ItemBuilder = Box<dyn FnMut(usize, &mut BuildCx<'_>)>`** — a cold path, called
  only when a row mounts.

### 5. `virtual_list::reconcile` — per frame, before Layout, gated to a no-op on steady scroll

The facade's `FramePhase::Layout` arm runs `reconcile` **before**
`relayout_and_paint`, then `absorb_measurements` after. Per list, reconcile is
gated: if `scroll().on(axis) == last_reconciled_scroll && !dirty_data`, it returns
immediately. Otherwise:

1. Read `scroll_main`; renormalize the anchor via `find_position(scroll_main)`.
2. Compute the visible span from the viewport extent; widen by `overscan` on each
   side, clamped to `[0, item_count]` → `(new_start, new_end)`.
3. If the window is unchanged and `!dirty_data`, return — **the steady path**. A
   scroll that stays within the mounted window is a pure ADR 0007 transform: the
   world rects already moved, reconcile rebinds nothing, marks no LAYOUT.
4. Otherwise diff the window: indices leaving push their host to the `pool`;
   indices entering pop a host from the pool (or allocate via the arena only if the
   pool is empty), `free_subtree` the old body, rebuild the body via the builder
   under the host, set `row_offset = prefix_sum(i)`, and mark the host subtree
   `MEASURE | LAYOUT | PAINT`. Rows staying in range whose `item_top` moved get
   their `row_offset` rewritten and are marked `LAYOUT | PAINT`.
5. If `total()` changed, rewrite the canvas's `Fixed` extent and mark it `MEASURE |
   LAYOUT`. Commit the window, clear `dirty_data`, store `last_reconciled_scroll`.

**Measurement feedback** (`absorb_measurements`, after relayout): sweep the frame's
newly-mounted hosts, read their measured main extent, feed `HeightTree::update` +
`HeightCache`; if any row's real height differed from its estimate, set
`dirty_data` so the next frame renormalizes the anchor — this is the scroll
correction that keeps an anchored row's on-screen top stable as variable heights
resolve. Bounded to the newly-mounted rows; no allocation.

### 6. Node recycle — a targeted detach + subtree-free API

Recycling needs to unmount a row's body without invalidating the host and without
bubbling `STRUCTURE`:

- `NodeArena::detach_child(child) → bool` — unlink a node from its parent and
  siblings, but **do not** free the slot or bump the generation. The host stays a
  valid `NodeId`, parked in the pool.
- `NodeStore::free_subtree(root, effects, scratch) → u32` — post-order free every
  **descendant** of `root` (the root itself stays parked), calling
  `effects.cancel_for_node` per freed node so scoped effects run their cleanup on
  unmount (§10.6). Freed slots return to the arena free list and are reused by the
  next `alloc` — bounded churn, no growth.
- `BuildCx::with_parent` seeds the builder so the item authors its body under a
  given host rather than the tree root; the body becomes the host's first child.

The pool is a flat `Vec<NodeId>` — one item template per list this slice.

### 7. Public API and facade wiring

```rust
pub struct VirtualListStyle { axis, size, overscan: u32, estimated_row: f32, style: BoxStyle }

impl BuildCx<'_> {
    pub fn virtual_list(
        &mut self,
        style: VirtualListStyle,
        item_count: usize,
        item: impl FnMut(usize, &mut BuildCx<'_>) + 'static,
    ) -> Handle;
}
```

`virtual_list` builds the Scroll viewport + the `AbsoluteRows` canvas, registers a
`VirtualListState` keyed by the viewport index, and mounts **no items at build** —
the first frame's reconcile mounts against the real viewport size. `AppDriver`
gains a `virtual_lists: VirtualLists` field, reset in `on_launch`, reconciled each
Layout phase.

Stable-key contract (§12.4): the logical index `i` is identity this slice — correct
for scroll and append; a row that stays in range keeps its host, its state, its
focus. A `key_of: Fn(usize) -> u64` hook is reserved on the style for reorder; a
full keyed reconciler is deferred to Phase 7. Input is untouched — the viewport is
a Scroll, so `ScrollRouter` already routes wheels and hit-test already narrows at
the viewport, touching only mounted rows.

## Consequences

- **§12.4 is met by construction**: a 100k-row list mounts ~visible + 2·overscan
  rows (~18–40, per viewport size), reports the full `scroll_range`, recycles a
  bounded handful on a crossing, and preserves the anchor across variable heights —
  all without a full-tree rebuild for a local scroll.
- **No new crate** (§3.3): the layout variant, the `row_offset` column, the
  registry, the reconcile, and the recycle API are all items in `viso-ui` +
  `viso`. `cargo xtask check-deps` stays at **13 crates** with no new edge.
  criterion is a `viso-ui` **dev-dependency** only — an external crate, never an
  architecture DAG edge, ignored by check-deps.
- **The dirty contract is load-bearing** (§11): the canvas being `Fixed` on both
  axes is what stops a remount's `LAYOUT` mark from bubbling to root. Change the
  canvas sizing and the virtualization budget breaks — this is why the reconcile
  marks `LAYOUT`, never `STRUCTURE`, and edits arena links directly.
- **Tradeoff vs Makepad's `PortalList`**: Viso trades a `Widget`-owned portal + an
  event-walk-driven range update for a driver-owned dense registry + a gated
  per-frame reconcile that is a no-op on steady scroll and O(window · log n) on a
  crossing. Smaller public mental model (§12.1 exposes a list, nothing about
  portals or turtles), cost that scales with the window and the log of the item
  count, not the item count. Port semantics, not the coarse model (§38.3).
- **Measured, not claimed** (§7.3 / §36): a release `large list` benchmark
  (`crates/ui/benches/large_list.rs`) drives the public reconcile → relayout →
  absorb seam over a 100k-row list. Baselines on this machine: a **steady
  within-row scroll frame ≈ 357 ns**, a **boundary-crossing frame ≈ 5.4 µs**. The
  bench's startup assertion (run before timing, failing the binary on regression)
  proves the steady path rebinds 0 rows and grows no reused scratch — the §7.1
  zero-allocation hot-path contract, exercised, not asserted in a comment.
- **Verified headlessly** (§69 / §66): check-deps (13 crates), fmt, clippy `-D
  warnings`, and the full `cargo test --workspace` suite (viso-ui unit tests
  including the height-tree round-trips, the Appendix C mount-window assertion, the
  steady 0-churn path, the 3-in/3-out recycle, variable-height anchor preservation,
  scroll-to-end clamp, item-count growth, the allocation guard, and the
  effect-cleanup-on-unmount test; plus three facade `virtual_list_seam` integration
  tests driving reconcile over a facade-built tree). No shader change, so no
  real-machine Metal pass was needed; no visual/Studio verification beyond the
  headless mount-count, scroll-range, and dirty-class assertions.
- **Known follow-ups, out of scope this pass**:
  - **Multi-template recycle pools** for heterogeneous rows — one flat pool this
    slice.
  - **Full keyed reconciliation** for mid-list insert/delete/reorder (the reserved
    `key_of` hook) — deferred to Phase 7's `widgets::List`.
  - **Virtual-range accessibility semantics** — how a screen reader sees 100k
    logical rows when only ~40 are mounted is its own slice.
