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

A grid track is sized by `TrackSizing { Fixed(f32), Fr(f32), Auto, Percent(f32),
Minmax(f32, f32), FitContent(f32) }`, defined in `crates/ui/src/grid.rs` — **not**
reusing the node `Length`
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
   start lies on that track). `Minmax(lo, hi)` → the content channel `auto_maxes[i]`
   clamped into `[lo, hi]` (an inverted band lets `lo` win, no panic); `FitContent(lim)`
   → `auto_maxes[i]` capped at `lim`, floored at 0. `Minmax` and `FitContent` are
   **fixed-size** content-sized tracks: they consume from `auto_maxes` in pass 1 and do
   **not** enter the pass-2 `Fr` sweep — the `Fr` sweep then splits whatever remains.
   Tally consumed extent and total `Fr` weight.
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

**Spanning-item Auto attribution:** span-1 children set each intrinsic track's baseline;
a spanning child then distributes its content across the growable tracks it covers
(`grid::distribute_spanning_auto`, CSS Grid §11.5 "distribute extra space to spanned
tracks"). The space a spanning item still needs — `measured − Σ track_prebase(covered) −
interior gaps`, floored at 0 — is spread equally over the growable (`Auto` / `Minmax` /
`FitContent`) tracks it covers and `max`-ed into each; `Fixed` / `Percent` / `Fr` tracks
absorb none of it (`track_prebase` accounts for their pre-solve extent, so only the
genuine shortfall is attributed). A span over no growable track grows nothing — its
content is honored by the cell rect, not by inflating a fixed track. This runs before
`solve_tracks`, as a second pass over the same `auto_maxes` channel, so the solver stays a
single linear pass with no fixed-point iteration.

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
  guard test over repeated whole-frame re-layout. **These have since been hoisted**
  into a reusable `GridScratch` checked out of a thread-local free-list pool (see the
  landed follow-up below), so a warmed steady-state grid re-layout now allocates
  nothing — pinned by the `grid_layout_alloc` counting-allocator test.
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
- **Landed follow-ups (Phase 8.7):**
  - `minmax()` / `fit-content()` track functions — added as `TrackSizing::Minmax(f32,
    f32)` / `FitContent(f32)` (still `Copy`), resolved in pass 1 as fixed-size
    content-sized tracks (see Decision 4). `repeat(count, inner)` is a **creation-time**
    template-expansion helper (`grid::repeat` / `repeated`), not a runtime `TrackSizing`
    variant — the solver only ever sees the flat expanded list. **Boundary:** `Minmax`'s
    `max` does not participate in `Fr` free-space distribution (CSS flexible-track
    semantics), and `repeat`'s `count` is a fixed integer here (`auto-fill`/`auto-fit`,
    which depend on the container extent, remain out of scope — §69 item 11). Verified by
    grid.rs solver unit tests (minmax lo/mid/hi clamp, inverted band, fit-content cap,
    fixed-track-then-Fr split, repeat≡hand-written) + layout.rs bounds goldens (minmax
    column clamps content and leaves the rest to Fr, floors small content at its min) +
    the `grid_relayout_12x20_minmax` bench baselining the new arms against the pure-Fr grid.
  - Spanning-item contribution to intrinsic track sizing (Decision 4 refinement) —
    `grid::distribute_spanning_auto` runs after the span-1 baseline pass and, for each
    item spanning >1 track, apportions its content shortfall equally across the growable
    (`Auto` / `Minmax` / `FitContent`) tracks it covers (see Decision 4). Verified by
    grid.rs unit tests (span-2 splits across two Auto columns, shortfall measured after a
    Fixed track, a narrow span adds nothing, interior gaps subtracted first, a span over
    only Fixed+Fr grows nothing, span-1 ignored) + a layout.rs bounds golden (a span-2
    item widens the two Auto columns it covers so the trailing Fr track lands at x=300).
  - Named grid lines and template-areas placement — both are **creation-time** naming
    features that lower to the existing numeric placement path, so the runtime carries no
    `String` (section 29). `GridStyle` gains three cold, optional fields:
    `column_line_names` / `row_line_names` (`LineNames` = `Vec<(Box<str>, u16)>`, a line
    name → 0-based line index table) and `areas: Option<GridAreas>` (a
    `grid-template-areas` table built once from a rectangular grid of area-name cells via
    `GridAreas::from_rows`, each name resolving to the bounding `CellRegion` of its cells;
    the `.` cell is the intentionally-empty marker). The facade resolves them while the
    grid closure runs: `BuildCx::place_named(col, row, col_span, row_span)` maps names
    through `grid::resolve_line` into an `Option<u16>` placement, and
    `BuildCx::place_area(name)` maps an area name into an explicit `GridPlacement`. The
    active grid's name tables are stashed on `BuildCx` (cold, boxed, saved/restored around
    nested grids) only when a grid declares names; an unknown name auto-flows that axis.
    **Runtime `place_children` and `GridPlacement` are unchanged** — pure `u16`. Verified by
    grid.rs unit tests (`resolve_line` hit/miss, `GridAreas::from_rows` column-span /
    both-axis span / `.` skip) + component.rs facade goldens (named line → column x,
    `place_area` single cell and multi-column span through full measure+layout).
  - Cell cross-axis alignment, including baseline — `GridStyle` gains
    `align_items: AlignItems { Stretch (default) / Start / Center / End / Baseline }`
    (the shared `layout::AlignItems`, a `Copy` scalar), carried on `LayoutInput::Grid`.
    `layout_grid` seats each child in its cell by this alignment on the block axis:
    `Stretch` grows a `Fill` child to the cell height (the previous implicit behavior),
    `Start` / `Center` / `End` hug the child's own measured height at the top / center /
    bottom, and `Baseline` shifts every cell in a row so their first-line baselines
    coincide. Baseline needs a per-child first-line baseline: `Content::Text` carries a
    `baseline: f32` field (`0` for an empty run), surfaced through
    `Content::baseline() -> Option<f32>` and the `content_baseline` layout hook; `Image` /
    `Path` report `None` and fall back to top alignment. A row's shared baseline is the max
    of its cells' baselines, and each child's block offset is `shared − child_baseline`.
    Verified by layout.rs unit tests (mixed-height text cells land on one baseline; a fixed
    child offsets under Start/Center/End and ignores Stretch; a `Fill` child grows only
    under Stretch) exercised through a new `alloc_leaf` + `set_content_payload` test path.
  - Subgrid (a grid child adopting its parent's tracks on one or both axes) — `GridStyle`
    gains two `Copy` scalars `subgrid_columns` / `subgrid_rows` (default `false`), carried on
    `LayoutInput::Grid` and read through the `subgrid_axes(index) -> (bool, bool)` layout hook.
    The mechanism is a **dedicated recursive entry**, not a change to the public `layout()`
    signature: `layout_grid` is parameterized with `inherited_cols` / `inherited_rows:
    Option<(&[f32], f32)>` (the parent's resolved track sizes over the child's cell span, plus
    the parent's gap on that axis). The public dispatcher always passes `(None, None)`; only the
    per-child loop passes inherited slices, and only for a grid child that declares itself a
    subgrid. On an inherited axis the child **skips** its own template build, auto-max collection,
    and `solve_tracks`, adopts the parent's sizes verbatim, uses the parent's gap for its
    prefix-offset math, and takes the inherited slice length as its authoritative column count for
    placement — so its inner cell lines coincide with the parent's grid lines **exactly, interior
    gaps included**. A non-subgrid axis self-solves as before (`None` degenerates to the prior
    path with zero behavior change and zero overhead on the common path). Rejected alternatives:
    threading inherited tracks through the whole `layout()` signature (taxes every call site and
    the hot dispatcher); overwriting the child's `GridTracks` template (a template cannot
    reproduce the parent's exact resolved Fr/Auto/Minmax line positions). Verified by layout.rs
    bounds goldens — a column-only subgrid reproducing the parent's gapped column lines, a
    span-at-offset subgrid adopting only the parent's `k..k+n` tracks, a both-axis subgrid landing
    every inner cell corner on the parent's lines, and a mixed-axis subgrid inheriting columns
    while self-solving its own Fr rows.
  - Per-grid-node `GridScratch` hoisting — `layout_grid` allocated ~12 per-call `Vec`s
    (placements, occupied bitset, cell regions, per-axis track / auto / size / offset buffers)
    every frame. These are now hoisted into a single `GridScratch` struct checked out of a
    **thread-local free-list pool** (`with_grid_scratch`): a call pops a buffer (or a fresh
    default), clears — does **not** free — its twelve `Vec`s, runs, and returns it to the pool.
    A pool (not one shared buffer) is required because `layout_grid` is **reentrant**: a subgrid
    child recurses into `layout_grid` from inside the parent's per-child loop while the parent
    still holds live slices of its own `col_sizes` / `row_sizes`, so a nested call must check out
    a *distinct* buffer — the pool grows only to the deepest grid nesting seen. `prefix_offsets`
    became `prefix_offsets_into` (writes a reused buffer instead of returning a fresh `Vec`). The
    UI tree / layout pass is main-thread-owned (§26), so the thread-local carries no locking.
    Verified by the `grid_layout_alloc` alloc pack (counting global allocator,
    `--test-threads=1`): a warmed steady-state 12×20 grid re-layout is **zero-alloc**. Same-machine
    A/B on `grid_relayout_12x20`: **~11.20 µs → ~10.44 µs** (criterion **−6.4%**, p < 0.05,
    "Performance has improved"). The prior `~8.33 µs` figure in Baseline was measured on a
    different machine/session and is not directly comparable; the A/B above is the authoritative
    before/after.
