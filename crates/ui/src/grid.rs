//! Grid layout: a two-dimensional track model (Fixed / Fr / Auto / Percent),
//! row/column spanning, and auto-flow-or-explicit placement. The public types
//! here describe a grid; the placement and track-solving algorithms below are
//! pure functions over plain slices so they test without a node store, and the
//! layout pass drives them through the warm side-columns on the node store.

use crate::layout::{Inset, Size};
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
// Consumed by the grid warm side-column in a later task.
#[allow(dead_code)]
pub(crate) struct GridTracks {
    /// Explicit column template.
    pub columns: Vec<TrackSizing>,
    /// Explicit row template.
    pub rows: Vec<TrackSizing>,
    /// Sizing rule for implicitly created rows.
    pub auto_rows: TrackSizing,
}

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
// Consumed by the grid layout pass in a later task.
#[allow(dead_code)]
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
        CellRegion {
            col: 0,
            row: 0,
            col_span: 1,
            row_span: 1,
        },
    );

    // Explicit children first: an item with both column and row pinned claims its
    // block (clamped into range) so auto-flow sees it as occupied.
    for (i, p) in placements.iter().enumerate() {
        if let (Some(c), Some(r)) = (p.column, p.row) {
            let col_span = p.column_span.max(1);
            let row_span = p.row_span.max(1);
            let col = c.min(cols.saturating_sub(1));
            let region = CellRegion {
                col,
                row: r,
                col_span: col_span.min(cols - col),
                row_span,
            };
            mark_block(
                occupied,
                cols,
                region.col,
                region.row,
                region.col_span,
                region.row_span,
            );
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

/// Resolve one axis's track extents. Fixed/Percent/Auto tracks take their value
/// directly; `Fr` tracks then split the remaining free space (content minus the
/// resolved non-Fr tracks minus the inter-track gaps) by a re-normalizing sweep:
/// each Fr track, resolved in order, takes `remaining_free * its_fr /
/// remaining_fr_total`, so the split stays exact as it proceeds. `auto_maxes[i]`
/// supplies the content size for an `Auto` track (0.0 elsewhere). Writes
/// `tracks.len()` extents into `out` (cleared first).
// Consumed by the grid layout pass in a later task.
#[allow(dead_code)]
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
                CellRegion {
                    col: 0,
                    row: 0,
                    col_span: 1,
                    row_span: 1
                },
                CellRegion {
                    col: 1,
                    row: 0,
                    col_span: 1,
                    row_span: 1
                },
                CellRegion {
                    col: 0,
                    row: 1,
                    col_span: 1,
                    row_span: 1
                },
            ]
        );
        assert_eq!(rows, 2);
    }

    #[test]
    fn explicit_placement_lands_exactly_and_auto_flows_around_it() {
        // Child 0 explicitly at (col 1, row 0); child 1 auto-flows → must take
        // (0,0), the free cell before the occupied one.
        let placements = [
            GridPlacement {
                column: Some(1),
                row: Some(0),
                column_span: 1,
                row_span: 1,
            },
            auto(1, 1),
        ];
        let mut occ = Vec::new();
        let mut out = Vec::new();
        place_children(2, &placements, &mut occ, &mut out);
        assert_eq!(
            out[0],
            CellRegion {
                col: 1,
                row: 0,
                col_span: 1,
                row_span: 1
            }
        );
        assert_eq!(
            out[1],
            CellRegion {
                col: 0,
                row: 0,
                col_span: 1,
                row_span: 1
            }
        );
    }

    #[test]
    fn a_span_two_item_occupies_two_cells_and_pushes_auto_flow() {
        // 2 columns. Child 0 auto span-2 → fills row 0 entirely; child 1 auto
        // span-1 → wraps to (0,1).
        let placements = [auto(2, 1), auto(1, 1)];
        let mut occ = Vec::new();
        let mut out = Vec::new();
        let rows = place_children(2, &placements, &mut occ, &mut out);
        assert_eq!(
            out[0],
            CellRegion {
                col: 0,
                row: 0,
                col_span: 2,
                row_span: 1
            }
        );
        assert_eq!(
            out[1],
            CellRegion {
                col: 0,
                row: 1,
                col_span: 1,
                row_span: 1
            }
        );
        assert_eq!(rows, 2);
    }

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
            &[
                TrackSizing::Fixed(200.0),
                TrackSizing::Fr(1.0),
                TrackSizing::Fr(2.0),
            ],
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
}
