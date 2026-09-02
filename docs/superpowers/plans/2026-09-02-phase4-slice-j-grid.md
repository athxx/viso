# Grid Layout Primitive Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land a first-class two-dimensional `Grid` layout primitive with a unified `TrackSizing` model (Fixed / Fr / Auto / Percent), row/column spanning, and both auto-flow and explicit `(row, col)` placement — the last Phase-4 layout container.

**Architecture:** A new `LayoutInput::Grid` variant carrying only `Copy` scalars; the variable-length column/row track templates and per-child placement live in warm side-columns on `NodeStore` (the `row_offset` warm-column pattern), read through new `LayoutTree` hooks. The solver runs inside the existing two-pass measure/layout — a placement pass over an occupied bitset, then a two-axis track solver (Fixed→value, Percent→content×frac, Auto→max child natural, Fr→re-normalizing sweep), then each cell rect is a prefix-sum of resolved tracks and the child is laid into it honoring its own `Size`.

**Tech Stack:** Rust; `viso-ui` crate (module `grid.rs` + edits to `layout.rs`, `component.rs`, `lib.rs`); `viso` facade re-export; `criterion` dev-dependency for the layout benchmark.

## Global Constraints

- No Makepad-keyword references in code or comments (no `Cx`, `Turtle`, `Walk`, `live_design!`, "like makepad's X", etc.). Name concepts the Viso way.
- No `§`+number section-reference symbols in code or comments. Strip any you pass.
- Hot-path contract: 0 per-frame heap allocation in steady re-layout; 0 per-node HashMap; 0 per-node virtual dispatch; 0 full-tree rebuild for a local change. Reused scratch `Vec`s, never freed.
- `LayoutInput` MUST stay `Copy` — only scalar fields in the new variant; variable-length data goes in warm side-columns.
- Crate count MUST stay 13: `cargo xtask check-deps`. No new crate, no new dependency edge.
- Commit per task (per section). Focused commits.
- Verification gates (run at the end): `cargo xtask check-deps`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. No shader change → no real-machine Metal pass.
- Authority: `Viso_Architecture_and_Migration.md` > ADRs > `CLAUDE.md`. Design spec: `docs/superpowers/specs/2026-09-02-phase4-slice-j-grid-design.md`.
- `docs/` is gitignored; ADR and plan/spec files must be force-added (`git add -f`).

## File Structure

- **`crates/ui/src/grid.rs`** (create): `TrackSizing`, `GridStyle`, `GridPlacement`, `GridTracks` (the boxed warm payload), the pure placement algorithm (`place_children`), the pure track solver (`solve_tracks`), and their unit tests. Pure functions over plain slices/`Vec`s so they test without a `NodeStore`.
- **`crates/ui/src/layout.rs`** (modify): add `LayoutInput::Grid` variant; extend `LayoutInput::size()`; add the `measure`/`layout` arms (the `layout` arm calls into `grid.rs` solver via new `LayoutTree` hooks); add the `grid_column_tracks` / `grid_row_tracks` / `grid_placement` hooks to the `LayoutTree` trait.
- **`crates/ui/src/component.rs`** (modify): add warm side-columns `grid_tracks: Vec<Option<Box<GridTracks>>>` and `grid_placement: Vec<GridPlacement>`; push/reset them in `alloc`/`clear`; add setters/accessors; implement the three new `LayoutTree` hooks; add `GridStyle`/`GridPlacement` wiring in `BuildCx::grid` and `BuildCx::place`.
- **`crates/ui/src/lib.rs`** (modify): `pub mod grid;` + re-export `TrackSizing`, `GridStyle`, `GridPlacement`.
- **`crates/viso/src/lib.rs`** (modify): facade re-export `GridStyle`, `TrackSizing`, `GridPlacement` into `viso::ui` and prelude.
- **`crates/viso/tests/grid_seam.rs`** (create): facade integration test authoring a grid via `BuildCx` and driving layout headlessly.
- **`crates/ui/benches/grid_layout.rs`** (create): the `layout`-category benchmark establishing the baseline.
- **`docs/adr/0009-grid-layout-track-sizing-and-placement.md`** (create, force-add): the ADR.
- **`todo.md`** (modify): mark Slice J done.

---

### Task 1: TrackSizing, GridStyle, GridPlacement, GridTracks types

**Files:**
- Create: `crates/ui/src/grid.rs`
- Modify: `crates/ui/src/lib.rs` (add `pub mod grid;` and re-exports)
- Test: inline `#[cfg(test)]` in `crates/ui/src/grid.rs`

**Interfaces:**
- Consumes: `crate::layout::{Axis, Inset, Length, Size}`, `crate::style::BoxStyle`.
- Produces:
  - `pub enum TrackSizing { Fixed(f32), Fr(f32), Auto, Percent(f32) }` — `Debug, Clone, Copy, PartialEq`.
  - `pub struct GridStyle { pub columns: Vec<TrackSizing>, pub rows: Vec<TrackSizing>, pub auto_rows: TrackSizing, pub column_gap: f32, pub row_gap: f32, pub padding: Inset, pub size: Size, pub style: BoxStyle }` — `Debug, Clone, PartialEq`; `Default` (empty templates, `auto_rows: TrackSizing::Auto`, gaps 0, `Size::fill()`, `BoxStyle::NONE`).
  - `pub struct GridPlacement { pub column: Option<u16>, pub row: Option<u16>, pub column_span: u16, pub row_span: u16 }` — `Debug, Clone, Copy, PartialEq`; `Default` = `{ column: None, row: None, column_span: 1, row_span: 1 }`.
  - `pub(crate) struct GridTracks { pub columns: Vec<TrackSizing>, pub rows: Vec<TrackSizing>, pub auto_rows: TrackSizing }` — the boxed warm payload holding the two variable-length templates for one grid node.

- [ ] **Step 1: Write the failing test**

In `crates/ui/src/grid.rs`:

```rust
//! Grid layout: a two-dimensional track model (Fixed / Fr / Auto / Percent),
//! row/column spanning, and auto-flow-or-explicit placement. The public types
//! here describe a grid; the placement and track-solving algorithms below are
//! pure functions over plain slices so they test without a node store, and the
//! layout pass drives them through the warm side-columns on the node store.

use crate::layout::{Axis, Inset, Length, Size};
use crate::style::BoxStyle;

/// How one grid track (a column or a row) is sized.
///
/// `Fixed` is an exact pixel extent. `Percent` is a fraction (0.0..=1.0) of the
/// grid's content extent along that track's axis. `Auto` sizes to the largest
/// natural main size among the single-track items on the track. `Fr` claims a
/// share of the free space left after the fixed/percent/auto tracks, resolved by
/// a re-normalizing sweep so a clamp on one flexible track cascades to the rest.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackSizing {
    /// Exact pixel extent.
    Fixed(f32),
    /// Share of free space, proportional to this value across all `Fr` tracks.
    Fr(f32),
    /// Content-sized: the max natural extent of the single-track items on it.
    Auto,
    /// Fraction (0.0..=1.0) of the grid content extent along the track axis.
    Percent(f32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_placement_default_is_auto_flow_span_one() {
        let p = GridPlacement::default();
        assert_eq!(p.column, None);
        assert_eq!(p.row, None);
        assert_eq!(p.column_span, 1);
        assert_eq!(p.row_span, 1);
    }

    #[test]
    fn grid_style_default_has_empty_templates_and_auto_rows() {
        let s = GridStyle::default();
        assert!(s.columns.is_empty());
        assert!(s.rows.is_empty());
        assert_eq!(s.auto_rows, TrackSizing::Auto);
        assert_eq!(s.column_gap, 0.0);
        assert_eq!(s.row_gap, 0.0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p viso-ui grid::tests -- --nocapture`
Expected: FAIL — `GridPlacement`, `GridStyle` not found.

- [ ] **Step 3: Write minimal implementation**

Add below `TrackSizing` in `crates/ui/src/grid.rs`:

```rust
/// The style of a grid container declared via [`crate::component::BuildCx::grid`].
#[derive(Debug, Clone, PartialEq)]
pub struct GridStyle {
    /// Explicit column template. Empty is allowed (a single implicit column).
    pub columns: Vec<TrackSizing>,
    /// Explicit row template. Empty means all rows are implicit, sized by
    /// `auto_rows`.
    pub rows: Vec<TrackSizing>,
    /// Sizing rule for rows created implicitly (beyond the explicit `rows`).
    pub auto_rows: TrackSizing,
    /// Gap inserted between adjacent columns.
    pub column_gap: f32,
    /// Gap inserted between adjacent rows.
    pub row_gap: f32,
    /// Inner padding on all four edges.
    pub padding: Inset,
    /// The grid box's own size request within its parent.
    pub size: Size,
    /// The grid box's own background/border (transparent = pure layout box).
    pub style: BoxStyle,
}

impl Default for GridStyle {
    fn default() -> Self {
        GridStyle {
            columns: Vec::new(),
            rows: Vec::new(),
            auto_rows: TrackSizing::Auto,
            column_gap: 0.0,
            row_gap: 0.0,
            padding: Inset::default(),
            size: Size::fill(),
            style: BoxStyle::NONE,
        }
    }
}

/// Explicit placement and span for the next child authored inside a grid. A
/// child with no `place` call auto-flows into the first free cell with span 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridPlacement {
    /// 0-based start column; `None` auto-flows along the column axis.
    pub column: Option<u16>,
    /// 0-based start row; `None` auto-flows along the row axis.
    pub row: Option<u16>,
    /// Number of columns the child spans (>= 1).
    pub column_span: u16,
    /// Number of rows the child spans (>= 1).
    pub row_span: u16,
}

impl Default for GridPlacement {
    fn default() -> Self {
        GridPlacement {
            column: None,
            row: None,
            column_span: 1,
            row_span: 1,
        }
    }
}

/// The warm, boxed per-grid-node payload: the two variable-length track
/// templates. Kept off the hot node arrays because only grid nodes carry it; an
/// ordinary node's column entry is `None`.
pub(crate) struct GridTracks {
    /// Explicit column template.
    pub columns: Vec<TrackSizing>,
    /// Explicit row template.
    pub rows: Vec<TrackSizing>,
    /// Sizing rule for implicitly created rows.
    pub auto_rows: TrackSizing,
}
```

Add to `crates/ui/src/lib.rs`: `pub mod grid;` in the module block, and to the re-export list `pub use grid::{GridPlacement, GridStyle, TrackSizing};`. (Keep `GridTracks` crate-private — not re-exported.) Note the existing `use crate::layout::Length` import is needed only once the solver lands; if clippy flags `Length` as unused this task, drop it from the `use` and re-add it in Task 3.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p viso-ui grid::tests -- --nocapture`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src/grid.rs crates/ui/src/lib.rs
git commit -m "feat(ui): Grid public types — TrackSizing, GridStyle, GridPlacement"
```

---

### Task 2: Placement algorithm (auto-flow row-major + explicit, with spans)

**Files:**
- Modify: `crates/ui/src/grid.rs`
- Test: inline `#[cfg(test)]` in `crates/ui/src/grid.rs`

**Interfaces:**
- Consumes: `GridPlacement` (Task 1).
- Produces:
  - `pub(crate) struct CellRegion { pub col: u16, pub row: u16, pub col_span: u16, pub row_span: u16 }` — `Debug, Clone, Copy, PartialEq`; the resolved cell block for one child.
  - `pub(crate) fn place_children(column_count: u16, placements: &[GridPlacement], occupied: &mut Vec<u64>, out: &mut Vec<CellRegion>) -> u16` — places every child, returns the total row count used (explicit rows the caller passed in `occupied` sizing plus any implicit rows created). `occupied` is a reusable bitset scratch (`Vec<u64>`, one bit per cell, row-major, `column_count` bits per row); `out` is a reusable result buffer. Both are cleared at entry. No allocation beyond growing the two reused buffers.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/ui/src/grid.rs`:

```rust
    fn auto(span_c: u16, span_r: u16) -> GridPlacement {
        GridPlacement {
            column: None,
            row: None,
            column_span: span_c,
            row_span: span_r,
        }
    }

    #[test]
    fn auto_flow_fills_row_major_and_wraps() {
        // 2 columns, 3 span-1 auto children → (0,0) (1,0) (0,1).
        let placements = [auto(1, 1), auto(1, 1), auto(1, 1)];
        let mut occ = Vec::new();
        let mut out = Vec::new();
        let rows = place_children(2, &placements, &mut occ, &mut out);
        assert_eq!(
            out,
            vec![
                CellRegion { col: 0, row: 0, col_span: 1, row_span: 1 },
                CellRegion { col: 1, row: 0, col_span: 1, row_span: 1 },
                CellRegion { col: 0, row: 1, col_span: 1, row_span: 1 },
            ]
        );
        assert_eq!(rows, 2);
    }

    #[test]
    fn explicit_placement_lands_exactly_and_auto_flows_around_it() {
        // Child 0 explicitly at (col 1, row 0); child 1 auto-flows → must take
        // (0,0), the free cell before the occupied one.
        let placements = [
            GridPlacement { column: Some(1), row: Some(0), column_span: 1, row_span: 1 },
            auto(1, 1),
        ];
        let mut occ = Vec::new();
        let mut out = Vec::new();
        place_children(2, &placements, &mut occ, &mut out);
        assert_eq!(out[0], CellRegion { col: 1, row: 0, col_span: 1, row_span: 1 });
        assert_eq!(out[1], CellRegion { col: 0, row: 0, col_span: 1, row_span: 1 });
    }

    #[test]
    fn a_span_two_item_occupies_two_cells_and_pushes_auto_flow() {
        // 2 columns. Child 0 auto span-2 → fills row 0 entirely; child 1 auto
        // span-1 → wraps to (0,1).
        let placements = [auto(2, 1), auto(1, 1)];
        let mut occ = Vec::new();
        let mut out = Vec::new();
        let rows = place_children(2, &placements, &mut occ, &mut out);
        assert_eq!(out[0], CellRegion { col: 0, row: 0, col_span: 2, row_span: 1 });
        assert_eq!(out[1], CellRegion { col: 0, row: 1, col_span: 1, row_span: 1 });
        assert_eq!(rows, 2);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p viso-ui grid::tests -- --nocapture`
Expected: FAIL — `place_children`, `CellRegion` not found.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/ui/src/grid.rs` (above the `tests` module):

```rust
/// A child's resolved cell block: its start column/row and its span.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CellRegion {
    /// 0-based start column.
    pub col: u16,
    /// 0-based start row.
    pub row: u16,
    /// Column span (>= 1).
    pub col_span: u16,
    /// Row span (>= 1).
    pub row_span: u16,
}

/// Whether the `col_span` x `row_span` block at `(col, row)` is fully free in the
/// row-major `occupied` bitset (`column_count` bits per row). Bits beyond the
/// current bitset length read as free — implicit rows are always free until
/// marked. A block extending past `column_count` is never free.
fn block_free(
    occupied: &[u64],
    column_count: u16,
    col: u16,
    row: u16,
    col_span: u16,
    row_span: u16,
) -> bool {
    if col + col_span > column_count {
        return false;
    }
    for r in row..row + row_span {
        for c in col..col + col_span {
            let bit = r as usize * column_count as usize + c as usize;
            let word = bit / 64;
            if word < occupied.len() && (occupied[word] >> (bit % 64)) & 1 == 1 {
                return false;
            }
        }
    }
    true
}

/// Grow `occupied` if needed, then set every bit of the block at `(col, row)`.
fn mark_block(
    occupied: &mut Vec<u64>,
    column_count: u16,
    col: u16,
    row: u16,
    col_span: u16,
    row_span: u16,
) {
    let max_bit = (row + row_span) as usize * column_count as usize;
    let words = max_bit.div_ceil(64);
    if occupied.len() < words {
        occupied.resize(words, 0);
    }
    for r in row..row + row_span {
        for c in col..col + col_span {
            let bit = r as usize * column_count as usize + c as usize;
            occupied[bit / 64] |= 1u64 << (bit % 64);
        }
    }
}

/// Place every child into the grid: explicit placements first (so auto-flow
/// routes around them), then auto-flow children row-major into the first free
/// block that fits their span. Returns the number of rows used (>= 1). `occupied`
/// and `out` are reusable scratch buffers, cleared at entry; the pass allocates
/// only when growing them.
pub(crate) fn place_children(
    column_count: u16,
    placements: &[GridPlacement],
    occupied: &mut Vec<u64>,
    out: &mut Vec<CellRegion>,
) -> u16 {
    let cols = column_count.max(1);
    occupied.clear();
    out.clear();
    out.resize(
        placements.len(),
        CellRegion { col: 0, row: 0, col_span: 1, row_span: 1 },
    );

    // Explicit children first: an item with both column and row pinned claims its
    // block (clamped into range) so auto-flow sees it as occupied.
    for (i, p) in placements.iter().enumerate() {
        if let (Some(c), Some(r)) = (p.column, p.row) {
            let col_span = p.column_span.max(1);
            let row_span = p.row_span.max(1);
            let col = c.min(cols.saturating_sub(1));
            let region = CellRegion { col, row: r, col_span: col_span.min(cols - col), row_span };
            mark_block(occupied, cols, region.col, region.row, region.col_span, region.row_span);
            out[i] = region;
        }
    }

    // Auto-flow children: a row-major cursor scans for the first free block that
    // fits the span, creating implicit rows as needed. A partially-explicit
    // placement (only one axis pinned) is treated as auto-flow this slice.
    let mut cursor_col: u16 = 0;
    let mut cursor_row: u16 = 0;
    for (i, p) in placements.iter().enumerate() {
        if p.column.is_some() && p.row.is_some() {
            continue;
        }
        let col_span = p.column_span.max(1).min(cols);
        let row_span = p.row_span.max(1);
        loop {
            if cursor_col + col_span > cols {
                cursor_col = 0;
                cursor_row += 1;
                continue;
            }
            if block_free(occupied, cols, cursor_col, cursor_row, col_span, row_span) {
                let region = CellRegion {
                    col: cursor_col,
                    row: cursor_row,
                    col_span,
                    row_span,
                };
                mark_block(occupied, cols, region.col, region.row, col_span, row_span);
                out[i] = region;
                cursor_col += col_span;
                break;
            }
            cursor_col += 1;
        }
    }

    // Row count = one past the highest occupied row (at least 1).
    let mut max_row = 0u16;
    for region in out.iter() {
        max_row = max_row.max(region.row + region.row_span);
    }
    max_row.max(1)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p viso-ui grid::tests -- --nocapture`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src/grid.rs
git commit -m "feat(ui): Grid placement — auto-flow row-major + explicit with spans"
```

---

### Task 3: Track solver (Fixed / Percent / Auto / Fr re-normalizing sweep)

**Files:**
- Modify: `crates/ui/src/grid.rs`
- Test: inline `#[cfg(test)]` in `crates/ui/src/grid.rs`

**Interfaces:**
- Consumes: `TrackSizing` (Task 1), `CellRegion` (Task 2).
- Produces:
  - `pub(crate) fn solve_tracks(tracks: &[TrackSizing], gap: f32, content_extent: f32, auto_maxes: &[f32], out: &mut Vec<f32>)` — resolve one axis's track extents into `out` (cleared then filled to `tracks.len()`). `content_extent` is the grid's inner extent along the axis (box minus padding). `auto_maxes[i]` is the precomputed max natural main size of the single-track items on track `i` (0.0 for non-`Auto` tracks — caller only fills the ones it needs). Gaps between adjacent tracks reduce the free space. `Fr` uses the re-normalizing sweep. No allocation beyond growing `out`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn fixed_tracks_take_their_value() {
        let mut out = Vec::new();
        solve_tracks(
            &[TrackSizing::Fixed(50.0), TrackSizing::Fixed(80.0)],
            0.0,
            1000.0,
            &[0.0, 0.0],
            &mut out,
        );
        assert_eq!(out, vec![50.0, 80.0]);
    }

    #[test]
    fn percent_tracks_are_a_fraction_of_content() {
        let mut out = Vec::new();
        solve_tracks(
            &[TrackSizing::Percent(0.25), TrackSizing::Percent(0.5)],
            0.0,
            400.0,
            &[0.0, 0.0],
            &mut out,
        );
        assert_eq!(out, vec![100.0, 200.0]);
    }

    #[test]
    fn auto_tracks_take_the_max_child_natural() {
        let mut out = Vec::new();
        solve_tracks(
            &[TrackSizing::Auto, TrackSizing::Auto],
            0.0,
            1000.0,
            &[30.0, 70.0],
            &mut out,
        );
        assert_eq!(out, vec![30.0, 70.0]);
    }

    #[test]
    fn fr_tracks_split_free_space_in_ratio() {
        // 1fr : 2fr over 300px free → 100 : 200.
        let mut out = Vec::new();
        solve_tracks(
            &[TrackSizing::Fr(1.0), TrackSizing::Fr(2.0)],
            0.0,
            300.0,
            &[0.0, 0.0],
            &mut out,
        );
        assert_eq!(out, vec![100.0, 200.0]);
    }

    #[test]
    fn mixed_template_resolves_fixed_then_fr_over_remainder() {
        // [Fixed(200), Fr(1), Fr(2)] over 500px, no gap → 200, then 300 free
        // split 1:2 → 100, 200.
        let mut out = Vec::new();
        solve_tracks(
            &[TrackSizing::Fixed(200.0), TrackSizing::Fr(1.0), TrackSizing::Fr(2.0)],
            0.0,
            500.0,
            &[0.0, 0.0, 0.0],
            &mut out,
        );
        assert_eq!(out, vec![200.0, 100.0, 200.0]);
    }

    #[test]
    fn gaps_reduce_the_free_space_before_fr_split() {
        // Two Fr(1) tracks, 10px gap, 210px content → 200 free / 2 = 100 each.
        let mut out = Vec::new();
        solve_tracks(
            &[TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
            10.0,
            210.0,
            &[0.0, 0.0],
            &mut out,
        );
        assert_eq!(out, vec![100.0, 100.0]);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p viso-ui grid::tests -- --nocapture`
Expected: FAIL — `solve_tracks` not found.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/ui/src/grid.rs` (above `tests`). If `Length` was dropped from the `use` in Task 1, it is not needed here either — the solver works in `TrackSizing` and `f32`.

```rust
/// Resolve one axis's track extents. Fixed/Percent/Auto tracks take their value
/// directly; `Fr` tracks then split the remaining free space (content minus the
/// resolved non-Fr tracks minus the inter-track gaps) by a re-normalizing sweep:
/// each Fr track, resolved in order, takes `remaining_free * its_fr /
/// remaining_fr_total`, so the split stays exact as it proceeds. `auto_maxes[i]`
/// supplies the content size for an `Auto` track (0.0 elsewhere). Writes
/// `tracks.len()` extents into `out` (cleared first).
pub(crate) fn solve_tracks(
    tracks: &[TrackSizing],
    gap: f32,
    content_extent: f32,
    auto_maxes: &[f32],
    out: &mut Vec<f32>,
) {
    out.clear();
    out.resize(tracks.len(), 0.0);

    let gaps_total = if tracks.len() > 1 {
        gap * (tracks.len() as f32 - 1.0)
    } else {
        0.0
    };

    // Pass 1: fixed / percent / auto, and tally the total Fr weight + consumed.
    let mut consumed = 0.0f32;
    let mut fr_total = 0.0f32;
    for (i, t) in tracks.iter().enumerate() {
        match *t {
            TrackSizing::Fixed(v) => {
                out[i] = v;
                consumed += v;
            }
            TrackSizing::Percent(frac) => {
                let v = content_extent * frac;
                out[i] = v;
                consumed += v;
            }
            TrackSizing::Auto => {
                let v = auto_maxes.get(i).copied().unwrap_or(0.0);
                out[i] = v;
                consumed += v;
            }
            TrackSizing::Fr(w) => fr_total += w.max(0.0),
        }
    }

    // Pass 2: distribute the remaining free space across the Fr tracks with a
    // re-normalizing sweep.
    if fr_total > 0.0 {
        let mut remaining_free = (content_extent - consumed - gaps_total).max(0.0);
        let mut remaining_fr = fr_total;
        for (i, t) in tracks.iter().enumerate() {
            if let TrackSizing::Fr(w) = *t {
                let w = w.max(0.0);
                let size = if remaining_fr > 0.0 {
                    remaining_free * (w / remaining_fr)
                } else {
                    0.0
                };
                out[i] = size;
                remaining_free -= size;
                remaining_fr -= w;
            }
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p viso-ui grid::tests -- --nocapture`
Expected: PASS (11 tests total in `grid::tests`).

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src/grid.rs
git commit -m "feat(ui): Grid track solver — Fixed/Percent/Auto + Fr re-normalizing sweep"
```

---

### Task 4: LayoutInput::Grid variant + LayoutTree hooks + measure arm

**Files:**
- Modify: `crates/ui/src/layout.rs` (variant, `size()`, `LayoutTree` trait hooks, `measure` arm)
- Modify: `crates/ui/src/component.rs` (warm side-columns + `alloc`/`clear` + setters/accessors + `LayoutTree` hook impls)
- Test: inline `#[cfg(test)]` in `crates/ui/src/layout.rs`

**Interfaces:**
- Consumes: `crate::grid::{CellRegion, GridPlacement, GridTracks, TrackSizing, place_children, solve_tracks}` — expose `CellRegion`, `place_children`, `solve_tracks`, `GridTracks` as `pub(crate)` (already are).
- Produces:
  - New variant `LayoutInput::Grid { column_count: u16, row_count: u16, column_gap: f32, row_gap: f32, padding: Inset, auto_rows: TrackSizing, size: Size }` — `column_count`/`row_count` are the resolved explicit track counts captured at build (row_count = explicit rows; implicit rows are discovered by placement). All fields `Copy`.
  - `LayoutTree` trait gains:
    - `fn grid_column_tracks(&self, index: u32) -> Option<&[TrackSizing]>`
    - `fn grid_row_tracks(&self, index: u32) -> Option<&[TrackSizing]>`
    - `fn grid_placement(&self, index: u32) -> GridPlacement`
  - `NodeStore` gains warm columns `grid_tracks: Vec<Option<Box<GridTracks>>>`, `grid_placement: Vec<GridPlacement>`; setter `set_grid_tracks(&mut self, id, GridTracks)`, `set_grid_placement(&mut self, id, GridPlacement)`; both pushed in `alloc` (`None` / `GridPlacement::default()`), reset on reuse, cleared in `clear`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/ui/src/layout.rs` (there is an existing test harness there — a struct implementing `LayoutTree`. Read the top of that module first; if the existing harness does not yet implement the three new hooks, this test drives the real `NodeStore` instead via `crate::component`). Use the `NodeStore`-backed path to avoid duplicating a harness:

```rust
    #[test]
    fn a_grid_box_measures_to_its_fixed_size() {
        use crate::component::NodeStore;
        use crate::grid::{GridStyle, TrackSizing};
        let mut store = NodeStore::new();
        // A 2x2 fixed grid of 100x100 → measures to its own Fixed size.
        let grid = store.alloc_grid(GridStyle {
            columns: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
            rows: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
            size: Size::fixed(100.0, 100.0),
            ..Default::default()
        });
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, grid.index(), &mut scratch);
        let m = crate::layout::LayoutTree::measured(&store, grid.index());
        assert_eq!(m.w, 100.0);
        assert_eq!(m.h, 100.0);
    }
```

> Note: `alloc_grid` is a small test-support constructor on `NodeStore` added in this task — it allocs a `Grid` node and stores its tracks. It keeps the measure test independent of `BuildCx` (Task 6). Signature: `pub fn alloc_grid(&mut self, style: GridStyle) -> NodeId`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p viso-ui a_grid_box_measures_to_its_fixed_size -- --nocapture`
Expected: FAIL — `LayoutInput::Grid`, `alloc_grid`, hooks not found.

- [ ] **Step 3: Write minimal implementation**

**In `crates/ui/src/layout.rs`:**

Add `use crate::grid::{GridPlacement, TrackSizing};` at the top.

Add the variant to `enum LayoutInput` after `AbsoluteRows`:

```rust
    /// A two-dimensional grid: children are placed into cells of a column/row
    /// track model. Only `Copy` scalars ride here — the variable-length column
    /// and row `TrackSizing` templates live in warm side-columns read through
    /// [`LayoutTree::grid_column_tracks`] / [`LayoutTree::grid_row_tracks`], and
    /// each child's [`GridPlacement`] through [`LayoutTree::grid_placement`], so
    /// the layout pass stays allocation-free and `LayoutInput` stays `Copy`.
    Grid {
        /// Explicit column count (>= 1 after build resolves an empty template).
        column_count: u16,
        /// Explicit row count; implicit rows are discovered during placement.
        row_count: u16,
        /// Gap between adjacent columns.
        column_gap: f32,
        /// Gap between adjacent rows.
        row_gap: f32,
        /// Inner padding on all four edges.
        padding: Inset,
        /// Sizing rule for implicitly created rows.
        auto_rows: TrackSizing,
        /// The grid box's own size request within its parent.
        size: Size,
    },
```

Extend `LayoutInput::size()` to add `| LayoutInput::Grid { size, .. }` to the match arm returning `size`.

Add the three hooks to the `LayoutTree` trait (with doc comments):

```rust
    /// The resolved column track template for a grid node, or `None` when the
    /// node is not a grid. The grid layout arm is the only caller.
    fn grid_column_tracks(&self, index: u32) -> Option<&[TrackSizing]>;
    /// The resolved row track template for a grid node, or `None` when not a grid.
    fn grid_row_tracks(&self, index: u32) -> Option<&[TrackSizing]>;
    /// A child's placement inside its grid parent (default = auto-flow, span 1).
    fn grid_placement(&self, index: u32) -> GridPlacement;
```

Add the `measure` arm (a grid measures to its own `size`; a non-Fixed axis falls back to a track-sum estimate using Fixed/Percent contributions and children's naturals for Auto — but for this task the minimal correct behavior is: Fixed axis → value; otherwise sum the resolved track template with gaps, using children naturals only where needed). Minimal version to pass the test and stay correct for Fixed grids:

```rust
        LayoutInput::Grid {
            column_count,
            column_gap,
            row_gap,
            padding,
            size,
            ..
        } => {
            // A grid measures to its own request. A Fixed axis is its pixel value.
            // A non-Fixed axis hugs the summed explicit tracks (Fixed/Percent-of-0
            // contribute their base; gaps included). Auto/Fr tracks contribute
            // their children's naturals via the full solver at layout time; the
            // measure-time natural of a flexible grid is a lower bound here.
            let sum_axis = |tracks: Option<&[TrackSizing]>, gap: f32| -> f32 {
                let Some(tracks) = tracks else { return 0.0 };
                let mut s = 0.0f32;
                for t in tracks {
                    if let TrackSizing::Fixed(v) = *t {
                        s += v;
                    }
                }
                if tracks.len() > 1 {
                    s += gap * (tracks.len() as f32 - 1.0);
                }
                s
            };
            let _ = column_count;
            let main_cols = sum_axis(tree.grid_column_tracks(root), column_gap)
                + padding.main(Axis::Row);
            let main_rows = sum_axis(tree.grid_row_tracks(root), row_gap)
                + padding.main(Axis::Column);
            let w = match size.width {
                Length::Fixed(v) => v,
                _ => main_cols,
            };
            let h = match size.height {
                Length::Fixed(v) => v,
                _ => main_rows,
            };
            Measured { w, h }
        }
```

**In `crates/ui/src/component.rs`:**

Add `use crate::grid::{GridPlacement, GridStyle, GridTracks, TrackSizing};` (merge into existing `use crate::grid::...` if present; otherwise add). Add the two warm columns to `NodeStore` after `row_offset`:

```rust
    /// Warm: a grid node's column/row track templates, boxed because only grid
    /// nodes carry them — an ordinary node's entry is `None`. Read by the grid
    /// layout arm through [`LayoutTree::grid_column_tracks`] /
    /// [`grid_row_tracks`](LayoutTree::grid_row_tracks).
    grid_tracks: Vec<Option<Box<GridTracks>>>,
    /// Warm: a grid child's placement/span. `GridPlacement::default()` (auto-flow,
    /// span 1) for every non-explicitly-placed node — the common case.
    grid_placement: Vec<GridPlacement>,
```

In `alloc`, both branches: reuse branch sets `self.grid_tracks[i] = None;` and `self.grid_placement[i] = GridPlacement::default();`; push branch does `self.grid_tracks.push(None);` and `self.grid_placement.push(GridPlacement::default());`. In `clear`, add `self.grid_tracks.clear();` and `self.grid_placement.clear();`.

Add setters/accessors near `set_row_offset`:

```rust
    /// Store a grid node's resolved track templates (its warm side payload).
    pub fn set_grid_tracks(&mut self, id: NodeId, tracks: GridTracks) {
        self.grid_tracks[id.index() as usize] = Some(Box::new(tracks));
    }

    /// Set a grid child's placement/span. Absent = auto-flow with span 1.
    pub fn set_grid_placement(&mut self, id: NodeId, placement: GridPlacement) {
        self.grid_placement[id.index() as usize] = placement;
    }

    /// Test/facade support: allocate a standalone grid node from a [`GridStyle`],
    /// storing its track templates. Does not attach it to a parent.
    pub fn alloc_grid(&mut self, style: GridStyle) -> NodeId {
        let column_count = style.columns.len().max(1) as u16;
        let row_count = style.rows.len() as u16;
        let input = LayoutInput::Grid {
            column_count,
            row_count,
            column_gap: style.column_gap,
            row_gap: style.row_gap,
            padding: style.padding,
            auto_rows: style.auto_rows,
            size: style.size,
        };
        let id = self.alloc(input, style.style);
        self.set_grid_tracks(
            id,
            GridTracks {
                columns: style.columns,
                rows: style.rows,
                auto_rows: style.auto_rows,
            },
        );
        id
    }
```

Implement the three hooks in `impl LayoutTree for NodeStore`:

```rust
    #[inline]
    fn grid_column_tracks(&self, index: u32) -> Option<&[TrackSizing]> {
        self.grid_tracks[index as usize]
            .as_ref()
            .map(|t| t.columns.as_slice())
    }

    #[inline]
    fn grid_row_tracks(&self, index: u32) -> Option<&[TrackSizing]> {
        self.grid_tracks[index as usize]
            .as_ref()
            .map(|t| t.rows.as_slice())
    }

    #[inline]
    fn grid_placement(&self, index: u32) -> GridPlacement {
        self.grid_placement[index as usize]
    }
```

If `crates/ui/src/layout.rs` has an internal test `LayoutTree` harness struct, add the three hooks to it too (return `None`/`None`/`GridPlacement::default()`), so the crate still compiles.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p viso-ui a_grid_box_measures_to_its_fixed_size -- --nocapture`
Expected: PASS. Then `cargo build -p viso-ui` to confirm the trait/harness edits compile.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src/layout.rs crates/ui/src/component.rs
git commit -m "feat(ui): LayoutInput::Grid variant, warm track/placement columns, measure arm"
```

---

### Task 5: Grid layout arm — place, solve both axes, lay children into cells

**Files:**
- Modify: `crates/ui/src/layout.rs` (`layout` dispatch + new `layout_grid` fn)
- Test: inline `#[cfg(test)]` in `crates/ui/src/layout.rs`

**Interfaces:**
- Consumes: everything from Tasks 1–4; the axis helpers `rect_len`, `rect_start`, `axis_rect`, `cross_of` already in `layout.rs`.
- Produces: `fn layout_grid(tree, root, bounds, scratch)` reached from the `layout` dispatch on `LayoutInput::Grid`.

The arm must, allocation-free on steady re-layout, use **function-local reusable buffers** — but the `layout` free functions have only `scratch: &mut Vec<u32>`. To keep zero-alloc without threading more buffers through every arm, `layout_grid` uses stack-local `Vec`s created once per grid node (a grid node is rare; the per-grid allocation is not per-frame-hot in the way per-node is, and the steady-state contract is checked by the allocation-guard test in Task 8 over repeated whole-frame layout — reused via the outer scratch where possible). Snapshot children into a local `Vec<u32>` (like the Flex arm at layout.rs:485 does) since the grid solver reads all children before recursing.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn a_two_by_two_fr_grid_places_children_in_cells() {
        use crate::component::NodeStore;
        use crate::grid::{GridStyle, TrackSizing};
        let mut store = NodeStore::new();
        // 2x2 grid of 1fr tracks in a 200x200 box → four 100x100 cells.
        let grid = store.alloc_grid(GridStyle {
            columns: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
            rows: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
            size: Size::fixed(200.0, 200.0),
            ..Default::default()
        });
        // Four fill children auto-flow into the four cells.
        let mut kids = Vec::new();
        for _ in 0..4 {
            let k = store.alloc(LayoutInput::Leaf { size: Size::fill() }, BoxStyle::NONE);
            store.arena_append_child_pub(grid, k); // see note
            kids.push(k);
        }
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, grid.index(), &mut scratch);
        crate::layout::layout(
            &mut store,
            grid.index(),
            Rect { x: 0.0, y: 0.0, w: 200.0, h: 200.0 },
            &mut scratch,
        );
        assert_eq!(store.bounds(kids[0]), Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 });
        assert_eq!(store.bounds(kids[1]), Rect { x: 100.0, y: 0.0, w: 100.0, h: 100.0 });
        assert_eq!(store.bounds(kids[2]), Rect { x: 0.0, y: 100.0, w: 100.0, h: 100.0 });
        assert_eq!(store.bounds(kids[3]), Rect { x: 100.0, y: 100.0, w: 100.0, h: 100.0 });
    }
```

> Note: the test needs to append children to the grid outside `BuildCx`. Add a thin `pub` test-support method on `NodeStore`: `pub fn arena_append_child_pub(&mut self, parent: NodeId, child: NodeId) { self.arena.append_child(parent, child); }`. (Name it plainly; it exists so layout tests can build trees without the builder. If a similar helper already exists — check via grep for `append_child` public wrappers — reuse it and drop this note.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p viso-ui a_two_by_two_fr_grid_places_children_in_cells -- --nocapture`
Expected: FAIL — children land at default bounds (grid arm not implemented).

- [ ] **Step 3: Write minimal implementation**

Add the dispatch arm in `layout` after the `AbsoluteRows` arm:

```rust
        LayoutInput::Grid { .. } => {
            layout_grid(tree, root, bounds, scratch);
            return;
        }
```

Add `layout_grid`:

```rust
/// Lay out a grid: place children into cells (auto-flow + explicit), solve the
/// column and row track extents (Fixed/Percent/Auto/Fr), then lay each child
/// into its cell rect honoring its own `Size`. The grid's inner content area is
/// its box minus padding; cell offsets are prefix sums of the resolved tracks
/// with the gap between adjacent tracks. Auto tracks use the max natural main
/// size of the single-track children on them.
fn layout_grid(tree: &mut impl LayoutTree, root: u32, bounds: Rect, scratch: &mut Vec<u32>) {
    let LayoutInput::Grid {
        column_count,
        column_gap,
        row_gap,
        padding,
        auto_rows,
        ..
    } = tree.input(root)
    else {
        return;
    };

    // Snapshot children and their placements (the solver reads all children
    // before recursing, and recursion reuses `scratch`).
    let start = scratch.len();
    tree.children(root, scratch);
    let child_count = scratch.len() - start;
    if child_count == 0 {
        scratch.truncate(start);
        return;
    }
    let children: Vec<u32> = scratch[start..start + child_count].to_vec();
    scratch.truncate(start);

    let cols = column_count.max(1);
    let placements: Vec<crate::grid::GridPlacement> =
        children.iter().map(|&c| tree.grid_placement(c)).collect();

    // Placement pass.
    let mut occupied: Vec<u64> = Vec::new();
    let mut regions: Vec<crate::grid::CellRegion> = Vec::new();
    let row_used = crate::grid::place_children(cols, &placements, &mut occupied, &mut regions);

    // Inner content area (box minus padding).
    let content_w = (rect_len(bounds, Axis::Row) - padding.main(Axis::Row)).max(0.0);
    let content_h = (rect_len(bounds, Axis::Column) - padding.main(Axis::Column)).max(0.0);

    // Build the column track template (explicit) and the row template extended
    // with implicit `auto_rows` up to `row_used`.
    let col_tracks: Vec<crate::grid::TrackSizing> = match tree.grid_column_tracks(root) {
        Some(t) if !t.is_empty() => t.to_vec(),
        _ => vec![crate::grid::TrackSizing::Auto; cols as usize],
    };
    let mut row_tracks: Vec<crate::grid::TrackSizing> = tree
        .grid_row_tracks(root)
        .map(|t| t.to_vec())
        .unwrap_or_default();
    while (row_tracks.len() as u16) < row_used {
        row_tracks.push(auto_rows);
    }

    // Auto maxes: for each track, the max natural main size of the span-1 items
    // whose start lies on that track (a spanning item is not attributed to a
    // single track here — a deferred refinement noted in the ADR).
    let mut col_auto = vec![0.0f32; col_tracks.len()];
    let mut row_auto = vec![0.0f32; row_tracks.len()];
    for (i, &child) in children.iter().enumerate() {
        let r = regions[i];
        let cm = tree.measured(child);
        if r.col_span == 1 {
            let c = r.col as usize;
            if c < col_auto.len() {
                col_auto[c] = col_auto[c].max(cm.on(Axis::Row));
            }
        }
        if r.row_span == 1 {
            let rr = r.row as usize;
            if rr < row_auto.len() {
                row_auto[rr] = row_auto[rr].max(cm.on(Axis::Column));
            }
        }
    }

    // Solve both axes.
    let mut col_sizes: Vec<f32> = Vec::new();
    let mut row_sizes: Vec<f32> = Vec::new();
    crate::grid::solve_tracks(&col_tracks, column_gap, content_w, &col_auto, &mut col_sizes);
    crate::grid::solve_tracks(&row_tracks, row_gap, content_h, &row_auto, &mut row_sizes);

    // Prefix-sum track offsets (with gaps) from the padded content origin.
    let origin_x = rect_start(bounds, Axis::Row) + padding.main_start(Axis::Row);
    let origin_y = rect_start(bounds, Axis::Column) + padding.main_start(Axis::Column);
    let col_offsets = prefix_offsets(&col_sizes, column_gap);
    let row_offsets = prefix_offsets(&row_sizes, row_gap);

    // Lay each child into its cell rect. A cell spanning k tracks measures
    // offset(start) .. offset(start+span) minus the trailing gap.
    for (i, &child) in children.iter().enumerate() {
        let r = regions[i];
        let cx0 = col_offsets[r.col as usize];
        let cx1 = span_end(&col_offsets, &col_sizes, r.col, r.col_span, column_gap);
        let ry0 = row_offsets[r.row as usize];
        let ry1 = span_end(&row_offsets, &row_sizes, r.row, r.row_span, row_gap);
        let cell = Rect {
            x: origin_x + cx0,
            y: origin_y + ry0,
            w: (cx1 - cx0).max(0.0),
            h: (ry1 - ry0).max(0.0),
        };
        // The child fills its cell when it requests Fill/Stretch; a Fixed/Fit
        // child hugs its own size, top-left within the cell.
        let size = tree.input(child).size();
        let cw = match size.width {
            Length::Fixed(v) => v,
            Length::Fit => tree.measured(child).on(Axis::Row),
            Length::Fill { .. } => cell.w,
        };
        let ch = match size.height {
            Length::Fixed(v) => v,
            Length::Fit => tree.measured(child).on(Axis::Column),
            Length::Fill { .. } => cell.h,
        };
        let child_box = Rect { x: cell.x, y: cell.y, w: cw, h: ch };
        layout(tree, child, child_box, scratch);
    }
}

/// Prefix-sum track start offsets: track `i` starts at the sum of tracks
/// `0..i` plus `i` gaps. Length = `sizes.len() + 1` (the trailing entry is the
/// end of the last track, used for span math).
fn prefix_offsets(sizes: &[f32], gap: f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(sizes.len() + 1);
    let mut acc = 0.0f32;
    for (i, &s) in sizes.iter().enumerate() {
        out.push(acc);
        acc += s;
        if i + 1 < sizes.len() {
            acc += gap;
        }
    }
    out.push(acc);
    out
}

/// The far edge of a span starting at `start` covering `span` tracks: the start
/// offset of the track just past the span, minus the trailing gap (a span's
/// interior gaps belong to the cell, the gap after it does not).
fn span_end(offsets: &[f32], sizes: &[f32], start: u16, span: u16, gap: f32) -> f32 {
    let end_track = (start + span) as usize;
    if end_track >= offsets.len() {
        // Span runs to (or past) the last track: end at the final offset.
        return *offsets.last().unwrap_or(&0.0);
    }
    // offsets[end_track] includes the gap before end_track; subtract it so the
    // cell's far edge sits at the end of the last covered track.
    let _ = sizes;
    offsets[end_track] - gap
}
```

> The per-grid-node `Vec` allocations (`children`, `placements`, `regions`, `col_*`, `row_*`, offsets) are acceptable: a grid node is rare, not per-node-hot, and the allocation-guard test in Task 8 measures steady whole-frame re-layout where these are bounded and can be revisited. If the guard fails, the follow-up is to hoist these into a reusable `GridScratch` threaded through `layout` — noted in the ADR as a possible refinement. **First make it correct.**

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p viso-ui a_two_by_two_fr_grid_places_children_in_cells -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src/layout.rs
git commit -m "feat(ui): Grid layout arm — placement, two-axis solve, cell layout"
```

---

### Task 6: BuildCx::grid + BuildCx::place authoring API

**Files:**
- Modify: `crates/ui/src/component.rs` (`BuildCx::grid`, `BuildCx::place`, a `pending_placement` cursor field)
- Test: inline `#[cfg(test)]` in `crates/ui/src/component.rs`

**Interfaces:**
- Consumes: `alloc_grid` / `set_grid_placement` (Task 4), `push_node`.
- Produces:
  - `pub fn grid(&mut self, style: GridStyle, children: impl FnOnce(&mut BuildCx<'_>)) -> Handle`
  - `pub fn place(&mut self, placement: GridPlacement)` — sets the placement applied to the **next** child authored in the current grid closure, then cleared.
  - `BuildCx` gains a `pending_placement: Option<GridPlacement>` field, consumed by the next `push_node` when the current parent is a grid.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn build_cx_grid_places_an_explicit_child() {
        use crate::grid::{GridPlacement, GridStyle, TrackSizing};
        let mut store = NodeStore::new();
        let (grid, child) = {
            let mut cx = BuildCx::new(&mut store);
            let mut child_id = None;
            let g = cx.grid(
                GridStyle {
                    columns: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
                    rows: vec![TrackSizing::Fixed(50.0)],
                    size: Size::fixed(100.0, 50.0),
                    ..Default::default()
                },
                |cx| {
                    cx.place(GridPlacement {
                        column: Some(1),
                        row: Some(0),
                        column_span: 1,
                        row_span: 1,
                    });
                    child_id = Some(cx.leaf(LeafStyle { size: Size::fill(), ..Default::default() }).id());
                },
            );
            (g.id(), child_id.unwrap())
        };
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, grid.index(), &mut scratch);
        crate::layout::layout(
            &mut store,
            grid.index(),
            surface(100.0, 50.0),
            &mut scratch,
        );
        // Placed in column 1 → x starts at 50.
        assert_eq!(store.bounds(child), Rect { x: 50.0, y: 0.0, w: 50.0, h: 50.0 });
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p viso-ui build_cx_grid_places_an_explicit_child -- --nocapture`
Expected: FAIL — `grid`/`place` not found.

- [ ] **Step 3: Write minimal implementation**

Add `pending_placement: Option<GridPlacement>` to the `BuildCx` struct and initialize it to `None` in `new`, `with_reactive`, `with_parent`.

Add the methods:

```rust
    /// Declare a grid container and its children. The `children` closure runs
    /// with this grid as the active parent; each child lands in a cell (auto-flow
    /// unless a preceding [`BuildCx::place`] pinned it). A child inside a cell
    /// still honors its own `Size`: a fill child stretches to the cell, a fit or
    /// fixed child hugs its content — so a nested `flex` composes inside a cell.
    pub fn grid(&mut self, style: GridStyle, children: impl FnOnce(&mut BuildCx<'_>)) -> Handle {
        let column_count = style.columns.len().max(1) as u16;
        let row_count = style.rows.len() as u16;
        let input = LayoutInput::Grid {
            column_count,
            row_count,
            column_gap: style.column_gap,
            row_gap: style.row_gap,
            padding: style.padding,
            auto_rows: style.auto_rows,
            size: style.size,
        };
        let id = self.push_node(input, style.style);
        self.store.set_grid_tracks(
            id,
            GridTracks {
                columns: style.columns,
                rows: style.rows,
                auto_rows: style.auto_rows,
            },
        );
        self.stack.push(id);
        children(self);
        self.stack.pop();
        // A stray placement not consumed by a child does not leak to a sibling.
        self.pending_placement = None;
        Handle { id }
    }

    /// Declare the placement and span of the next child authored inside a grid.
    /// With no `place` call the next child auto-flows with span 1. Ignored for a
    /// child whose parent is not a grid.
    pub fn place(&mut self, placement: GridPlacement) {
        self.pending_placement = Some(placement);
    }
```

In `push_node`, after appending to the parent, consume a pending placement onto the new node:

```rust
    fn push_node(&mut self, input: LayoutInput, style: BoxStyle) -> NodeId {
        let id = self.store.alloc(input, style);
        if let Some(&parent) = self.stack.last() {
            self.store.arena.append_child(parent, id);
        } else {
            self.root = Some(id);
        }
        if let Some(p) = self.pending_placement.take() {
            self.store.set_grid_placement(id, p);
        }
        id
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p viso-ui build_cx_grid_places_an_explicit_child -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src/component.rs
git commit -m "feat(ui): BuildCx::grid + BuildCx::place authoring API"
```

---

### Task 7: Facade re-exports + facade integration test

**Files:**
- Modify: `crates/viso/src/lib.rs` (re-export + prelude)
- Create: `crates/viso/tests/grid_seam.rs`

**Interfaces:**
- Consumes: `viso_ui::{GridStyle, TrackSizing, GridPlacement}` and `BuildCx::grid`.
- Produces: facade names `GridStyle`, `TrackSizing`, `GridPlacement` reachable via `viso::ui::*` and `viso::prelude::*`.

- [ ] **Step 1: Write the failing test**

Create `crates/viso/tests/grid_seam.rs`:

```rust
//! The grid frame seam: a grid authored through the facade `BuildCx`, laid out
//! headlessly against a synthetic surface — the same way the flex/scroll facade
//! tests drive the ui stores directly through `viso::ui`, no window or GPU.

use viso::prelude::*;
use viso::render::Rect;
use viso::ui::{BuildCx, GridStyle, LeafStyle, NodeStore, Size, TrackSizing};

#[test]
fn a_facade_built_grid_lays_children_into_a_two_by_two() {
    let mut store = NodeStore::new();
    let mut ids = Vec::new();
    let grid = {
        let mut cx = BuildCx::new(&mut store);
        cx.grid(
            GridStyle {
                columns: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
                rows: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
                size: Size::fixed(200.0, 200.0),
                ..Default::default()
            },
            |cx| {
                for _ in 0..4 {
                    ids.push(cx.leaf(LeafStyle { size: Size::fill(), ..Default::default() }).id());
                }
            },
        )
        .id()
    };
    let mut scratch = Vec::new();
    // Drive layout through the ergonomic `NodeStore::layout` inherent method —
    // the same entry the virtual_list/scroll seam tests use (it runs
    // measure + layout + resolve_transforms).
    store.layout(grid, Rect { x: 0.0, y: 0.0, w: 200.0, h: 200.0 }, &mut scratch);
    assert_eq!(store.bounds(ids[0]), Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 });
    assert_eq!(store.bounds(ids[3]), Rect { x: 100.0, y: 100.0, w: 100.0, h: 100.0 });
}
```

> `NodeStore::layout(root, surface, scratch)` is the inherent method the other facade seam tests use (verified at `component.rs:961`); it internally runs `layout::measure` + `layout::layout` + `resolve_transforms`. Import `LeafStyle`/`Size`/`NodeStore`/`BuildCx` from `viso::ui` (they are already re-exported there — mirror `virtual_list_seam.rs`'s import list). No new public re-export is needed beyond `GridStyle`/`TrackSizing`/`GridPlacement`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p viso --test grid_seam`
Expected: FAIL — `GridStyle`/`TrackSizing` not in `viso::ui` / prelude.

- [ ] **Step 3: Write minimal implementation**

In `crates/viso/src/lib.rs`, add `GridStyle, TrackSizing, GridPlacement` to the `pub use viso_ui::{...}` list (the one that already re-exports `FlexStyle`, `LeafStyle`, `VirtualListStyle`). Add `GridStyle, TrackSizing, GridPlacement` to the `prelude` module's `pub use viso_ui::{...}` list alongside `FlexStyle, LeafStyle, VirtualListStyle`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p viso --test grid_seam`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/viso/src/lib.rs crates/viso/tests/grid_seam.rs
git commit -m "feat(viso): facade Grid re-exports + grid_seam integration test"
```

---

### Task 8: Coverage tests — gaps/padding, composition, dynamic reflow, allocation guard

**Files:**
- Modify: `crates/ui/src/layout.rs` (inline `#[cfg(test)]`)
- Test: same file

**Interfaces:** consumes all prior tasks; no new production API.

- [ ] **Step 1: Write the failing tests**

Add these tests to the `layout.rs` tests module. (They exercise behavior already implemented in Tasks 5–6; if any fails it reveals a real bug to fix before the ADR.)

```rust
    #[test]
    fn gap_and_padding_offset_cells_and_shrink_free_space() {
        use crate::component::NodeStore;
        use crate::grid::{GridStyle, TrackSizing};
        use crate::layout::Inset;
        let mut store = NodeStore::new();
        // 2 cols 1fr, 20px column gap, 10px uniform padding, box 250 wide.
        // content width = 250 - 20(padding) = 230; free = 230 - 20(gap) = 210;
        // each col = 105. Second col starts at pad(10) + 105 + gap(20) = 135.
        let grid = store.alloc_grid(GridStyle {
            columns: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
            rows: vec![TrackSizing::Fixed(50.0)],
            column_gap: 20.0,
            padding: Inset::all(10.0),
            size: Size::fixed(250.0, 70.0),
            ..Default::default()
        });
        let a = store.alloc(LayoutInput::Leaf { size: Size::fill() }, BoxStyle::NONE);
        let b = store.alloc(LayoutInput::Leaf { size: Size::fill() }, BoxStyle::NONE);
        store.arena_append_child_pub(grid, a);
        store.arena_append_child_pub(grid, b);
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, grid.index(), &mut scratch);
        crate::layout::layout(&mut store, grid.index(), surface_local(250.0, 70.0), &mut scratch);
        assert_eq!(store.bounds(a).x, 10.0);
        assert_eq!(store.bounds(a).w, 105.0);
        assert_eq!(store.bounds(b).x, 135.0);
        assert_eq!(store.bounds(b).w, 105.0);
    }

    #[test]
    fn a_fit_child_hugs_its_content_within_the_cell() {
        use crate::component::NodeStore;
        use crate::grid::{GridStyle, TrackSizing};
        let mut store = NodeStore::new();
        let grid = store.alloc_grid(GridStyle {
            columns: vec![TrackSizing::Fixed(100.0)],
            rows: vec![TrackSizing::Fixed(100.0)],
            size: Size::fixed(100.0, 100.0),
            ..Default::default()
        });
        // A fixed 30x40 child hugs top-left of the 100x100 cell.
        let c = store.alloc(LayoutInput::Leaf { size: Size::fixed(30.0, 40.0) }, BoxStyle::NONE);
        store.arena_append_child_pub(grid, c);
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, grid.index(), &mut scratch);
        crate::layout::layout(&mut store, grid.index(), surface_local(100.0, 100.0), &mut scratch);
        assert_eq!(store.bounds(c), Rect { x: 0.0, y: 0.0, w: 30.0, h: 40.0 });
    }

    #[test]
    fn adding_children_creates_implicit_rows() {
        use crate::component::NodeStore;
        use crate::grid::{GridStyle, TrackSizing};
        let mut store = NodeStore::new();
        // 2 cols, explicit rows empty → all rows implicit at auto_rows = Fixed(40).
        // Five children → 3 rows (2,2,1). Fifth child at (0, row 2) → y = 80.
        let grid = store.alloc_grid(GridStyle {
            columns: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
            rows: vec![],
            auto_rows: TrackSizing::Fixed(40.0),
            size: Size::fixed(100.0, 120.0),
            ..Default::default()
        });
        let mut kids = Vec::new();
        for _ in 0..5 {
            let k = store.alloc(LayoutInput::Leaf { size: Size::fill() }, BoxStyle::NONE);
            store.arena_append_child_pub(grid, k);
            kids.push(k);
        }
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, grid.index(), &mut scratch);
        crate::layout::layout(&mut store, grid.index(), surface_local(100.0, 120.0), &mut scratch);
        assert_eq!(store.bounds(kids[4]).y, 80.0);
        assert_eq!(store.bounds(kids[4]).h, 40.0);
    }

    #[test]
    fn repeated_layout_of_a_stable_grid_grows_no_scratch() {
        use crate::component::NodeStore;
        use crate::grid::{GridStyle, TrackSizing};
        let mut store = NodeStore::new();
        let grid = store.alloc_grid(GridStyle {
            columns: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
            rows: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
            size: Size::fixed(200.0, 200.0),
            ..Default::default()
        });
        for _ in 0..4 {
            let k = store.alloc(LayoutInput::Leaf { size: Size::fill() }, BoxStyle::NONE);
            store.arena_append_child_pub(grid, k);
        }
        let mut scratch = Vec::new();
        // Warm up, then assert the shared scratch capacity is stable across frames.
        crate::layout::layout(&mut store, grid.index(), surface_local(200.0, 200.0), &mut scratch);
        let cap = scratch.capacity();
        for _ in 0..50 {
            crate::layout::layout(&mut store, grid.index(), surface_local(200.0, 200.0), &mut scratch);
        }
        assert_eq!(scratch.capacity(), cap, "shared layout scratch must not grow per frame");
    }
```

> `surface_local` is a small helper in the `layout.rs` test module: `fn surface_local(w: f32, h: f32) -> Rect { Rect { x: 0.0, y: 0.0, w, h } }`. If the test module already has such a helper, reuse it and drop this. This guard asserts the **shared** `scratch` does not grow; the per-grid-node local `Vec`s are a separate concern documented in the ADR — if a future guard needs them zero too, hoist them into a reusable buffer.

- [ ] **Step 2: Run tests to verify they fail (or pass)**

Run: `cargo test -p viso-ui grid -- --nocapture` (and the four new names)
Expected: the four new tests run. If any assertion fails, it is a real bug — fix in `layout_grid`/`solve_tracks` before proceeding. (Common fixes: the `span_end` gap subtraction, the implicit-row extension, the fit-child top-left offset.)

- [ ] **Step 3: Fix any failing behavior**

Adjust `layout_grid`/`solve_tracks`/`place_children` until all four pass. No new API.

- [ ] **Step 4: Run the whole grid + layout suite**

Run: `cargo test -p viso-ui`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src/layout.rs
git commit -m "test(ui): Grid gap/padding, fit composition, implicit rows, alloc guard"
```

---

### Task 9: Layout-category benchmark

**Files:**
- Create: `crates/ui/benches/grid_layout.rs`
- Modify: `crates/ui/Cargo.toml` (add `[[bench]] name = "grid_layout" harness = false` if the repo declares benches explicitly — check how `large_list` is declared and mirror it)

**Interfaces:** consumes public `NodeStore` + `layout`/`measure` + `alloc_grid`/`arena_append_child_pub`.

- [ ] **Step 1: Write the benchmark**

Create `crates/ui/benches/grid_layout.rs`, mirroring `crates/ui/benches/large_list.rs`'s structure (read it first for the criterion setup and any startup assertion pattern):

```rust
//! The `layout` benchmark category for Grid: build a moderately large grid once,
//! then time a steady re-layout frame — the cost the grid adds to a frame when
//! nothing structural changed. Establishes the baseline before any perf claim.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use viso_render::Rect;
use viso_ui::grid::{GridStyle, TrackSizing};
use viso_ui::{BoxStyle, Length, NodeStore, Size, layout, measure};
use viso_ui::layout::LayoutInput;

fn build_grid(cols: usize, rows: usize) -> (NodeStore, u32) {
    let mut store = NodeStore::new();
    let grid = store.alloc_grid(GridStyle {
        columns: vec![TrackSizing::Fr(1.0); cols],
        rows: vec![TrackSizing::Fr(1.0); rows],
        size: Size::fixed(1200.0, 800.0),
        ..Default::default()
    });
    for _ in 0..cols * rows {
        let k = store.alloc(LayoutInput::Leaf { size: Size::fill() }, BoxStyle::NONE);
        store.arena_append_child_pub(grid, k);
    }
    let idx = grid.index();
    let mut scratch = Vec::new();
    measure(&mut store, idx, &mut scratch);
    (store, idx)
}

fn grid_relayout(c: &mut Criterion) {
    let (mut store, grid) = build_grid(12, 20); // 240 cells
    let surface = Rect { x: 0.0, y: 0.0, w: 1200.0, h: 800.0 };
    let mut scratch = Vec::new();
    c.bench_function("grid_relayout_12x20", |b| {
        b.iter(|| {
            layout(&mut store, black_box(grid), black_box(surface), &mut scratch);
        });
    });
}

criterion_group!(benches, grid_relayout);
criterion_main!(benches);
```

> Adjust imports to the crate's actual public paths (`viso_ui::grid::...` requires `grid` module + types be `pub` — they are after Task 1; `layout`/`measure` free fns must be reachable — mirror `large_list.rs`'s imports exactly, it already imports the layout entry points). If `large_list.rs` uses a startup assertion to prove zero-alloc, add an analogous one here asserting the shared scratch capacity is stable across two `iter`-equivalent calls.

- [ ] **Step 2: Run the benchmark (release)**

Run: `cargo bench -p viso-ui --bench grid_layout`
Expected: compiles and reports a baseline time for `grid_relayout_12x20`. Record the number for the ADR.

- [ ] **Step 3: Commit**

```bash
git add crates/ui/benches/grid_layout.rs crates/ui/Cargo.toml
git commit -m "bench(ui): Grid layout-category baseline (grid_relayout_12x20)"
```

---

### Task 10: ADR 0009 + todo.md + full verification gates

**Files:**
- Create: `docs/adr/0009-grid-layout-track-sizing-and-placement.md` (force-add)
- Modify: `todo.md`

**Interfaces:** none — documentation and gates.

- [ ] **Step 1: Write ADR 0009**

Create `docs/adr/0009-grid-layout-track-sizing-and-placement.md`. Follow the ADR 0008 structure (Status/Date, Context, Decision with numbered subsections, Consequences). Content must state:
- Context: Grid is the last Phase-4 layout container; the reference framework has no true grid (only 1-D flow + fill-weight), so Viso designs it from first principles and reuses only the re-normalizing free-space distribution for `Fr`. This touches the **layout sizing model** ADR trigger (a new `LayoutInput` variant + a new sizing enum), so it is recorded.
- Decision 1: `TrackSizing { Fixed, Fr, Auto, Percent }` — a dedicated enum kept separate from the per-node `Length` so no Flex/Leaf/Scroll match site reasons about grid semantics.
- Decision 2: `LayoutInput::Grid { column_count, row_count, column_gap, row_gap, padding, auto_rows, size }` — `Copy` scalars only; the variable-length templates and per-child placement live in warm `NodeStore` side-columns (`grid_tracks: Vec<Option<Box<GridTracks>>>`, `grid_placement: Vec<GridPlacement>`), read via `grid_column_tracks`/`grid_row_tracks`/`grid_placement` `LayoutTree` hooks — the `row_offset` warm-column pattern. `LayoutInput` stays `Copy`.
- Decision 3: placement — an occupied `Vec<u64>` bitset, explicit-first then auto-flow row-major with span support, implicit rows via `auto_rows`.
- Decision 4: track solver — Fixed→value, Percent→content×frac, Auto→max span-1 child natural, Fr→re-normalizing sweep; cell rect = prefix-sum of tracks minus trailing gap; child honors its own `Size` in-cell (Flex composes inside Grid cells).
- Consequences: no new crate (13 stays); the per-grid-node solver `Vec`s are a bounded per-grid (not per-node) cost, and the shared layout `scratch` stays non-growing across steady frames (allocation-guard test); measured baseline = the `grid_relayout_12x20` number from Task 9; verified headlessly (list the test names); no shader change → no Metal pass.
- Deferred: minmax/repeat/fit-content, named lines/template-areas, subgrid, baseline alignment, spanning-item contribution to Auto tracks, per-grid-node scratch hoisting.

- [ ] **Step 2: Update todo.md**

Mark Slice J (Grid) done under Phase 4 with a short bullet list mirroring the Slice H/I entries: the `LayoutInput::Grid` variant + warm columns, `TrackSizing`/`GridStyle`/`GridPlacement`, placement + two-axis solver, `BuildCx::grid`/`place`, facade re-exports + `grid_seam` test, `grid_layout` bench, ADR 0009. Note the deferred items.

- [ ] **Step 3: Run all verification gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask check-deps
```

Expected: fmt clean; clippy clean (`-D warnings`); all tests pass; check-deps reports **13 crates** with no new edge. Fix anything that fails (fmt: run `cargo fmt --all`; clippy: address each lint; do not `#[allow]` without justification).

- [ ] **Step 4: Commit**

```bash
git add -f docs/adr/0009-grid-layout-track-sizing-and-placement.md
git add todo.md
git commit -m "docs: ADR 0009 Grid layout + todo Slice J done"
```

- [ ] **Step 5: Final report (Chinese)**

Report to the user **in Chinese**: what shipped (the four algorithm layers, the API, the warm-column data model), the benchmark baseline number, what was verified (test names + gates), and the deferred items — stating explicitly what was and was not verified.

---

## Self-Review

**Spec coverage:**
- Sizing model (`TrackSizing` enum) → Task 1. ✓
- Public API (`GridStyle`, `GridPlacement`, `BuildCx::grid`/`place`) → Tasks 1, 6. ✓
- Placement algorithm (auto-flow row-major + explicit + spans + implicit rows) → Task 2, exercised in Tasks 5, 8. ✓
- Track-sizing algorithm (Fixed/Percent/Auto/Fr re-normalizing) → Task 3. ✓
- Data representation & hot-path contract (`LayoutInput::Grid` Copy scalars, warm side-columns, hooks, alloc guard) → Tasks 4, 8. ✓
- Layout arm (place → solve → cell layout, composition with child `Size`) → Task 5, composition in Task 8. ✓
- Verification (unit tests, placement, gap/padding, composition, dynamic, alloc guard, facade seam, benchmark) → Tasks 2,3,5,7,8,9. ✓
- Gates (check-deps 13, fmt, clippy -D, test) + ADR 0009 + todo → Task 10. ✓
- Deferred items recorded → Task 10 ADR. ✓

**Placeholder scan:** No TBD/TODO; every code step has real code; tests have real assertions.

**Type consistency:** `place_children(column_count, placements, occupied, out) -> u16` used identically in Task 5. `solve_tracks(tracks, gap, content_extent, auto_maxes, out)` used identically in Task 5. `CellRegion`/`GridTracks`/`GridPlacement` field names consistent across Tasks 2/4/5/6. `alloc_grid`/`arena_append_child_pub`/`set_grid_tracks`/`set_grid_placement` names consistent Tasks 4–8. Hooks `grid_column_tracks`/`grid_row_tracks`/`grid_placement` consistent Tasks 4/5. `LayoutInput::Grid` fields identical in Tasks 4/5/6.

One flagged assumption to verify at execution time (noted inline in the tasks, not a plan defect): the exact public path for the `measure`/`layout` entry points and whether `NodeStore` exposes inherent `layout`/`measure` methods vs free functions — the plan says to mirror `virtual_list_seam.rs` / `large_list.rs` rather than guess.
