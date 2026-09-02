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
