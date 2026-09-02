# Phase 4 · Slice J — Grid layout primitive

- Date: 2026-09-02
- Status: Design approved, ready for implementation plan
- Authority: `Viso_Architecture_and_Migration.md` §22 (public layout primitives + length units), §12 (layout rules). Makepad is an algorithm reference only (§38.3).

## Goal

Land the last Phase-4 layout primitive: a first-class **Grid**. Viso already has
Flex (Row/Column, Fill-weight, Fit, align, gap/padding), Scroll, and VirtualList.
Grid completes the set with a unified two-dimensional track model, so that a
"flexible / elastic / dynamic" layout — the user's stated need — has a single,
data-oriented primitive instead of hand-nested rows and columns.

## Why Grid, and why this shape (three-way comparison)

**Makepad has no Grid at all.** All 2D layout goes through the Turtle's
`Flow::{Right, Down, Overlay}` plus `Size::Fill { weight }` (a 1-D flex factor).
A true grid must be hand-built by nesting Rows/Columns, or via `data_grid`, a
spreadsheet widget that computes explicit pixel column widths itself. What is
worth porting from Makepad is its `resolve_fill` **re-normalizing free-space
distribution**: each flexible track is sized against the space remaining after the
already-resolved tracks, proportional to its share of the *remaining* flex weight,
so min/max clamping on one track cascades correctly. Viso reuses that algorithm
for `Fr` track sizing and supplies the 2D grid Makepad lacks.

**Flutter has a grid, but fragmented.** `GridView` (equal/ratio cells), `Table`
(hand-computed column widths), `Wrap` (flow), `StaggeredGrid` — the user must pick
a widget per case, and mixed templates like `[Px(200), Fr(1), Fr(2)]` require
nesting Row + Expanded by hand. Flutter's layout also runs on heap-allocated
`RenderObject`s with per-node virtual `performLayout` dispatch, and `setState`
tends to rebuild subtrees via Element diff.

**Viso's advantage:** one `Grid` primitive with a unified `TrackSizing` model
(Fixed / Fr / Auto / Percent) covering the whole GridView+Wrap+Table+nested-Flex
surface; a **data-oriented** layout (SoA `NodeStore`, generational `NodeId`, zero
per-node virtual dispatch) with the track template in a warm side-column so the
hot layout pass stays allocation-free; a retained tree with precise dirty classes
so a local state change never rebuilds the tree. Small public mental model,
specialized internal algorithm, performance first — the three architecture goals.

## Sizing model

A dedicated track-sizing enum, kept separate from the per-node box `Length`
(Fixed/Fill/Fit) so no existing Flex/Leaf/Scroll match site has to reason about
grid-only semantics:

```rust
pub enum TrackSizing {
    Fixed(f32),    // exact px extent
    Fr(f32),       // fraction of free space (re-normalizing sweep, from Makepad resolve_fill)
    Auto,          // content-sized: max natural main size of the items on that track
    Percent(f32),  // fraction (0.0..=1.0) of the grid content extent along the track axis
}
```

## Public API (facade `BuildCx`)

```rust
pub struct GridStyle {
    pub columns: Vec<TrackSizing>,   // explicit column template
    pub rows: Vec<TrackSizing>,      // explicit row template; empty => implicit rows via auto_rows
    pub auto_rows: TrackSizing,      // sizing rule for implicitly created rows
    pub column_gap: f32,
    pub row_gap: f32,
    pub padding: Inset,
    pub size: Size,                  // the grid box's own request (Length)
    pub style: BoxStyle,
}

pub struct GridPlacement {           // optional explicit placement of the next child
    pub column: Option<u16>,         // 0-based start column; None => auto-flow
    pub row: Option<u16>,            // 0-based start row; None => auto-flow
    pub column_span: u16,            // default 1
    pub row_span: u16,               // default 1
}
impl Default for GridPlacement { /* column: None, row: None, spans: 1 */ }

impl BuildCx<'_> {
    pub fn grid(&mut self, style: GridStyle, children: impl FnOnce(&mut BuildCx<'_>)) -> Handle;
    /// Declare placement/span for the NEXT child authored in a grid's closure.
    /// Not called => that child auto-flows with span 1.
    pub fn place(&mut self, placement: GridPlacement);
}
```

Children are authored as direct children of the grid (like Flex). Each child lands
in a cell; inside its cell it still honors its own `Size` (a `Fill` child stretches
to the cell, a `Fit` child hugs its content), so **Flex nests inside Grid cells** —
the two elastic models compose.

## Placement algorithm (auto-flow row-major + explicit)

1. An `occupied` sparse bitset (`Vec<u64>`, `columns × rows_used`; no HashMap).
2. **Explicit children first**: mark their `row_span × column_span` block occupied.
   Out-of-range/conflicting placements follow a deterministic fallback (advance to
   the next free block), so the result is testable.
3. **Auto-flow children**: a row-major cursor scans for the first free block that
   fits `column_span × row_span`; place and mark. When the current row can't fit,
   wrap to a new row (implicit rows created under `auto_rows`).
4. Output per child: `(start_col, start_row, col_span, row_span)`, written to a
   warm side-column read by the layout pass.

## Track-sizing algorithm (two-pass; reuses the existing measure/layout)

Column tracks (at layout time, after the bottom-up measure pass):
1. `Fixed` → its value.
2. `Percent` → `content_extent × fraction`.
3. `Auto` → max measured main size of the span-1 items on that column; a spanning
   item distributes its excess across the Auto tracks it covers (CSS rule).
4. `Fr` → `free = content_extent − Σ(fixed/percent/auto) − total_gap`, then the
   Makepad-style re-normalizing sweep: each Fr track = `remaining_free × its_fr /
   remaining_fr_total`, resolved in order so clamping cascades.

Row tracks resolve identically (implicit rows via `auto_rows`; an `Auto` row is the
max measured height of its row's items).

Placement: each cell rect = prefix-sum offset of its start track through its end
track, minus the gap on the trailing edge; the child is laid into that rect and its
own `Size` aligns it within the cell.

## Data representation & hot-path contract

- `LayoutInput::Grid { column_count, row_count, column_gap, row_gap, padding, auto_rows, size }`
  — only `Copy` scalars. `LayoutInput` stays `Copy`; `LayoutInput::size()` extends
  to the new variant.
- The variable-length `columns`/`rows` `TrackSizing` templates and the per-child
  placement live in **warm side-columns on `NodeStore`**, keyed by node index (the
  same pattern as `row_offset` from the VirtualList slice), read through new
  `LayoutTree` hooks (`grid_column_tracks`, `grid_row_tracks`, `grid_placement`).
  The layout pass borrows the slices — no per-node HashMap, no allocation.
- Two reusable scratch `Vec<f32>` (resolved column widths, row heights) + the reused
  occupied bitset. Steady re-layout of an unchanged grid is allocation-free.
- Dirty behavior: template/placement/child-size changes mark `MEASURE | LAYOUT`;
  they do not bubble `STRUCTURE`. A Grid box that is `Fixed` on both axes contains
  its children's layout invalidation, consistent with the dirty-propagation rule.

## Verification (headless)

Drive `NodeStore` + `layout` directly with a synthetic surface `Rect` (as the Flex
and Scroll tests do); no GPU needed.

- `TrackSizing` resolution unit tests: Fixed exact; Percent of content; Fr split
  (1:2 → 1/3 : 2/3); Auto = max child natural; mixed `[Px(200), Fr(1), Fr(2)]`;
  Fr re-normalization when a Fixed/Auto track consumes space first.
- Placement: auto-flow fills row-major and wraps; explicit `(row,col)` lands
  exactly; a span-2 item occupies two cells and pushes auto-flow around it;
  conflict fallback is deterministic.
- Gap/padding: gaps subtract from free space and offset cells; padding insets the
  content area.
- Composition: a `Fill` child stretches to its cell; a `Fit`/fixed child hugs;
  nested Flex inside a cell lays out correctly.
- Dynamic: growing/shrinking the child count reflows the grid (new implicit rows
  appear) without a full-tree rebuild.
- Allocation guard: repeated layout of a stable grid grows no scratch capacity.
- A facade integration test in `crates/viso/tests/` authoring a grid via `BuildCx`.
- A `layout`-category benchmark in `crates/ui/benches/` establishing the baseline
  before any perf claim (per the benchmark requirement).

Then the standard gates: `cargo xtask check-deps` (must stay 13 crates),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`. No shader change → no real-machine Metal pass. Commit per
section. Write **ADR 0009** (layout sizing-model change: the `Grid` LayoutInput
variant, `TrackSizing`, the warm track/placement side-columns, the two-axis track
solver) and update `todo.md`.

## Files

**Create:** `crates/ui/src/grid.rs` (TrackSizing, GridStyle, GridPlacement, the
placement + track-solving algorithms, unit tests). Possibly fold the solver into
`layout.rs` if it reads more naturally there — decided during implementation.

**Modify:** `crates/ui/src/layout.rs` (`LayoutInput::Grid` variant + measure/layout
arms + `size()` + the `grid_*` `LayoutTree` hooks); `crates/ui/src/component.rs`
(warm track/placement side-columns + `LayoutTree` impl + setters; `BuildCx::grid` +
`GridStyle` + `BuildCx::place`); `crates/ui/src/lib.rs` (`mod grid;` + re-exports);
`crates/viso/src/lib.rs` / prelude (facade `Grid` name via re-export).

## Deferred (noted, out of scope)

- `minmax(a, b)` tracks, `repeat()` shorthand, `fit-content()`.
- Named grid lines / template areas.
- Subgrid.
- Baseline alignment across cells.

Each is a later slice; the primitive shipped here is the flexible, dynamic,
two-dimensional base they build on.
