# ADR 0009 — Grid layout: track sizing, placement, and the two-axis solver

- Status: Accepted
- Date: 2026-09-02

## Context

Grid is the last of Phase 4's layout containers (Scroll → VirtualList → Grid).
Unlike Scroll and VirtualList, the reference framework has **no true grid** — its
"grid-like" surfaces are hand-built nested flex rows. So Viso designs Grid from
first principles against the architecture doc (`Viso_Architecture_and_Migration.md`
§68/§69 item 11): a two-dimensional track model with explicit and auto-flow
placement, spanning, and four track units.

The one idea reused from the reference is narrow and deliberate: the
**re-normalizing free-space sweep** for distributing the flexible (`Fr`) share —
resolving each flexible track in order against the *remaining* free space and
*remaining* flexible weight, so a clamp on one track cascades exactly to the rest.
Everything else — the placement bitset, the four-unit solver, the cell-rect prefix
math, the composition of a child's own `Size` inside its cell — is Viso's own.

Grid touches the layout **sizing-model** ADR trigger (§68): it introduces a new
`LayoutInput` variant with its own warm side-columns and a new sizing enum kept
separate from `Length`. This ADR records those decisions.

## Decision

### 1. `TrackSizing` is a separate enum from `Length`

A grid track is sized by `TrackSizing { Fixed(f32), Fr(f32), Auto, Percent(f32) }`,
defined in `crates/ui/src/grid.rs` — **not** reusing the node `Length`
(`Fixed`/`Fill`/`Fit`). The two look similar (`Fr` ~ `Fill`, `Auto` ~ `Fit`) but
mean different things: `Length` is one node's request within one parent's single
main axis; `TrackSizing` is a *column-or-row template slot* resolved against the
grid's content extent, with `Percent` (a fraction of that extent) that has no
`Length` analogue. Keeping them separate means **no `Length` match site** — none
of the Flex / Leaf / Scroll / AbsoluteRows arms — ever has to reason about grid
semantics, and a future minmax/repeat extension stays contained to `TrackSizing`.

A child *inside* a cell still uses its own `Length` `Size`: a `Fill` child stretches
to the cell, a `Fixed`/`Fit` child hugs its content at the cell's top-left. So a
nested `flex` composes inside a grid cell with no special case.

### 2. `LayoutInput::Grid` carries only `Copy` scalars; templates live warm

`LayoutInput` stays `Copy` (the layout pass reads it by value, alloc-free). The Grid
variant therefore carries only fixed-width scalars:

```
Grid { column_count: u16, row_count: u16, column_gap: f32, row_gap: f32,
       padding: Inset, auto_rows: TrackSizing, size: Size }
```

The variable-length data rides in two warm side-columns on `NodeStore`, keyed by
node index:

- `grid_tracks: Vec<Option<Box<GridTracks>>>` — the column/row `TrackSizing`
  templates + `auto_rows`, boxed because only grid nodes carry them (an ordinary
  node's entry is `None`).
- `grid_placement: Vec<GridPlacement>` — a child's `{ column, row, column_span,
  row_span }`, defaulting to auto-flow span-1 for the common (non-placed) child.

The layout pass reads them through three `LayoutTree` hooks —
`grid_column_tracks(index) -> Option<&[TrackSizing]>`, `grid_row_tracks(index)`,
and `grid_placement(index) -> GridPlacement`. This is the same warm-side-column
pattern established for Scroll (`scroll_offset`) and VirtualList (`row_offset`):
`LayoutInput` stays `Copy`, the hot arrays stay lean, and the layout pass stays
registry-free.

### 3. Placement: explicit-first, then auto-flow row-major, with implicit rows

`grid::place_children(column_count, placements, occupied, out) -> row_count` runs a
two-phase placement over a row-major `occupied` bitset (`Vec<u64>`, `column_count`
bits per row; bits past the current length read free, so implicit rows are free
until marked):

1. **Explicit first** — every child with *both* `column` and `row` pinned claims its
   (clamped) block, marking `occupied`, so auto-flow routes around it.
2. **Auto-flow** — a row-major cursor scans for the first free block that fits the
   child's span, creating implicit rows as needed. A partially-pinned child (only
   one axis given) is treated as auto-flow this slice.

The returned `row_count` is one past the highest occupied row (≥ 1). Rows beyond the
explicit `rows` template are **implicit**, sized by `auto_rows` — the layout arm
extends the row template with `auto_rows` up to `row_count`.

### 4. Two-axis track solver, then cell-rect prefix math

`grid::solve_tracks(tracks, gap, content_extent, auto_maxes, out)` resolves one axis
in two passes:

1. **Pass 1** — `Fixed` → its value; `Percent(frac)` → `content_extent * frac`;
   `Auto` → `auto_maxes[i]` (the max natural main size of the *span-1* children whose
   start lies on that track). Tally consumed extent and total `Fr` weight.
2. **Pass 2** — the re-normalizing `Fr` sweep: `remaining_free = content_extent −
   consumed − gaps_total`, then each `Fr(w)` track in order takes
   `remaining_free * w / remaining_fr`, decrementing both — so the split stays exact
   as it proceeds and a zero/clamped track cascades correctly.

The layout arm calls `solve_tracks` once per axis, then computes **prefix-sum offsets**
(`prefix_offsets`): track `i` starts at the sum of tracks `0..i` plus `i` gaps, with a
trailing entry marking the end of the last track. A cell spanning `k` tracks measures
`offset(start) .. span_end(start, span)` where `span_end` is the offset of the track
just past the span **minus the trailing gap** (a span's interior gaps belong to the
cell; the gap *after* it does not). Each child is then laid into its cell rect honoring
its own `Size` (see Decision 1).

**Spanning-item Auto attribution:** a spanning child's natural size is *not* attributed
to any single `Auto` track in this slice — only span-1 children contribute to `Auto`
maxes. This keeps the solver a single linear pass; the refinement (distributing a
spanning item's excess across its Auto tracks) is deferred.

## Consequences

- **No new crate.** Grid lives in the existing `viso-ui` crate as `grid.rs` +
  arms in `layout.rs` + authoring API in `component.rs`. `cargo xtask check-deps`
  stays at **13 crates**.
- **Authoring API** (§7 builder): `BuildCx::grid(style, children)` declares the
  container; `BuildCx::place(placement)` pins the *next* child authored in the grid
  closure (cleared after consumption, never leaking to a sibling). A stray unused
  placement is dropped when the closure ends. Facade re-exports `GridStyle`,
  `TrackSizing`, `GridPlacement` through `viso_ui`; the `grid_seam` test guards the
  re-export path.
- **Allocation shape.** The per-grid-node solver buffers (`children`, `placements`,
  `regions`, `col_*`/`row_*` sizes, offsets) are `Vec`s allocated *per grid node*,
  not per ordinary node and not per frame per child. A grid node is rare; these are
  bounded by that grid's track and child counts, and the non-growing steady-state
  contract (§7.1) is checked by the `repeated_layout_of_a_stable_grid_grows_no_scratch`
  guard test over repeated whole-frame re-layout. If a future profile shows this
  matters, the follow-up is to hoist these into a reusable `GridScratch` threaded
  through the `layout` pass — noted below, not done now (correctness first).
- **Baseline.** `cargo bench -p viso-ui --bench grid_layout` establishes
  `grid_relayout_12x20` (a 12×20 = 240-cell Fr grid, full re-layout) at a **~8.33 µs
  median** (measured `[8.13 µs, 8.33 µs, 8.51 µs]`). This is the number any later
  perf claim must beat; no perf claim is made without it. The bench also asserts the
  scratch capacity does not grow across repeated frames.
- **Verified headlessly** (no GPU; `NodeStore` + free layout fns over synthetic
  `Rect`s, per §35/§69):
  - `grid.rs` unit tests (11): placement defaults, auto-flow row-major wrap, explicit
    placement routing auto-flow around it, span-2 push, and the six solver cases
    (fixed / percent / auto / fr-ratio / mixed-fixed-then-fr / gaps-reduce-free).
  - `layout.rs` arm tests (6): `a_grid_box_measures_to_its_fixed_size`,
    `a_two_by_two_fr_grid_places_children_in_cells`,
    `gap_and_padding_offset_cells_and_shrink_free_space`,
    `a_fit_child_hugs_its_content_within_the_cell`,
    `adding_children_creates_implicit_rows`,
    `repeated_layout_of_a_stable_grid_grows_no_scratch`.
  - `component.rs`: `build_cx_grid_places_an_explicit_child` (authoring API).
  - facade: `grid_seam` (re-export surface).
- **No shader change** → no real-machine Metal pass needed this slice.
- **Known follow-ups, out of scope:**
  - `minmax()`, `repeat()`, `fit-content()` track functions (contained to `TrackSizing`).
  - Named grid lines and template-areas placement.
  - Subgrid (a grid child adopting its parent's tracks).
  - Baseline alignment of cell contents.
  - Spanning-item contribution to `Auto` track sizing (Decision 4 refinement).
  - Per-grid-node `GridScratch` hoisting if a profile ever demands it.
