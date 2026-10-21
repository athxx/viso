//! Grid layout: a two-dimensional track model (Fixed / Fr / Auto / Percent),
//! row/column spanning, and auto-flow-or-explicit placement. The public types
//! here describe a grid; the placement and track-solving algorithms below are
//! pure functions over plain slices so they test without a node store, and the
//! layout pass drives them through the warm side-columns on the node store.

use crate::layout::{AlignItems, Inset, Size};
use crate::style::BoxStyle;
pub use viso_schema::layout::{AdaptiveColumns, AutoRepeat, GridPlacement, TrackMax, TrackSizing};

/// Expand a `repeat(count, inner)` track function into `count` copies of `inner`,
/// appended to `out`. `repeat` is a creation-time template macro, not a runtime
/// track type: the grid solver only ever sees the expanded, flat track list, so a
/// `repeat(3, Fr(1))` column template is byte-for-byte a hand-written
/// `[Fr(1), Fr(1), Fr(1)]`. A `count` of 0 appends nothing.
pub fn repeat(count: u16, inner: TrackSizing, out: &mut Vec<TrackSizing>) {
    out.reserve(count as usize);
    for _ in 0..count {
        out.push(inner);
    }
}

/// Convenience form of [`repeat`] that returns a fresh `Vec` — for building a
/// column/row template inline (`columns: repeated(4, TrackSizing::Fr(1.0))`).
pub fn repeated(count: u16, inner: TrackSizing) -> Vec<TrackSizing> {
    let mut out = Vec::with_capacity(count as usize);
    repeat(count, inner, &mut out);
    out
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
    /// Block-axis (vertical) alignment of each cell's content within its cell.
    /// `Stretch` (the default) makes a `Fill`-height child fill its cell; the
    /// other modes position a hugged child at the top/center/bottom, and
    /// `Baseline` aligns cells in a row on their shared text baseline.
    pub align_items: AlignItems,
    /// The grid box's own background/border (transparent = pure layout box).
    pub style: BoxStyle,
    /// Author-declared column line names, resolved to 0-based line indices at
    /// build time. Cold: consumed only while authoring a named `place` inside the
    /// grid closure, never on the layout hot path. Empty for the common grid.
    pub column_line_names: LineNames,
    /// Author-declared row line names (see [`GridStyle::column_line_names`]).
    pub row_line_names: LineNames,
    /// Optional template-areas table (CSS `grid-template-areas`). Cold: a
    /// `place_area` call resolves a name to an explicit placement at build time.
    /// `None` for the common grid.
    pub areas: Option<GridAreas>,
    /// This grid is a subgrid on the column (inline) axis: when it is itself a
    /// child cell of another grid, it adopts that parent grid's resolved column
    /// tracks over its cell span instead of solving its own `columns` template,
    /// so its inner column lines coincide exactly with the parent's. Ignored
    /// when the grid is not a child of another grid. Default `false`.
    pub subgrid_columns: bool,
    /// This grid is a subgrid on the row (block) axis (see `subgrid_columns`).
    pub subgrid_rows: bool,
    /// A responsive `repeat(auto-fill | auto-fit, minmax(min, max))` column
    /// template: the *number* of columns is computed from the container's inner
    /// width during layout rather than fixed by `columns`. `Some` replaces the
    /// explicit `columns` template on the inline axis (rows are unaffected).
    /// Cold: a fixed-length `Copy` scalar with `None` on the common grid, so the
    /// non-adaptive path pays nothing. Ignored on an axis inherited by a subgrid.
    pub adaptive_columns: Option<AdaptiveColumns>,
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
            align_items: AlignItems::Stretch,
            style: BoxStyle::NONE,
            column_line_names: Vec::new(),
            row_line_names: Vec::new(),
            areas: None,
            subgrid_columns: false,
            subgrid_rows: false,
            adaptive_columns: None,
        }
    }
}

/// A creation-time table mapping author line names to 0-based grid line indices.
///
/// A "line" is the boundary between two tracks: an axis with `n` tracks has
/// `n + 1` lines (0 = the leading edge, `n` = the trailing edge). Names are
/// resolved to indices **once, at build time** (see [`resolve_line`]) so the
/// runtime placement path never carries a `String` — a named `place` call lowers
/// to the same `Option<u16>` a numeric `place` produces.
///
/// The natural authoring form is the CSS `[name]` line marker interleaved with
/// the track template; here the resolved `(name, index)` pairs are supplied
/// directly, so the same list serves columns and rows.
pub type LineNames = Vec<(Box<str>, u16)>;

/// Resolve a line name against a [`LineNames`] table, returning its 0-based line
/// index. A creation-time helper: called while authoring a grid child's
/// placement, never on the layout hot path. Returns `None` for an unknown name.
pub fn resolve_line(names: &[(Box<str>, u16)], name: &str) -> Option<u16> {
    names
        .iter()
        .find(|(candidate, _)| candidate.as_ref() == name)
        .map(|(_, index)| *index)
}

/// A creation-time table mapping template-area names to the cell block each area
/// covers. Built once from a rectangular grid of area-name cells (CSS
/// `grid-template-areas`) via [`GridAreas::from_rows`]; each distinct name
/// becomes the bounding [`CellRegion`] of the cells that carry it. Like
/// [`LineNames`] this is a cold, build-time structure — `place_area` resolves a
/// name to an explicit [`GridPlacement`] before the node is stored, so the
/// runtime placement path stays `String`-free.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GridAreas {
    /// `(area name, bounding cell block)`, one entry per distinct named area.
    areas: Vec<(Box<str>, CellRegion)>,
}

impl GridAreas {
    /// Build an area table from a rectangular grid of area-name cells, row-major
    /// (`rows[r][c]` is the area name occupying cell `(col = c, row = r)`). Each
    /// distinct name resolves to the **bounding block** of every cell carrying it,
    /// so `[["nav", "nav"], ["side", "main"]]` gives `nav` a span-2 column block on
    /// row 0, `side` a single cell at `(0, 1)`, and `main` at `(1, 1)`.
    ///
    /// A well-formed template names a contiguous rectangle per area; a
    /// non-rectangular or gapped name still resolves to its bounding block (the
    /// smallest rectangle covering all its cells) rather than panicking — malformed
    /// input degrades to a defined placement instead of a build failure. The `.`
    /// cell name marks an intentionally empty cell and is never registered as an
    /// area. Rows of differing length are honored as authored (a short row simply
    /// contributes fewer cells).
    pub fn from_rows<R, C>(rows: R) -> Self
    where
        R: IntoIterator<Item = C>,
        C: IntoIterator,
        C::Item: AsRef<str>,
    {
        // Accumulate each name's min/max column and row as cells are visited, then
        // fold those extents into a bounding CellRegion. Insertion order of first
        // appearance is preserved so resolution is deterministic.
        let mut bounds: Vec<(Box<str>, u16, u16, u16, u16)> = Vec::new();
        for (r, cols) in rows.into_iter().enumerate() {
            for (c, name) in cols.into_iter().enumerate() {
                let name = name.as_ref();
                if name == "." || name.is_empty() {
                    continue;
                }
                let (col, row) = (c as u16, r as u16);
                if let Some(entry) = bounds.iter_mut().find(|(n, ..)| n.as_ref() == name) {
                    entry.1 = entry.1.min(col);
                    entry.2 = entry.2.max(col);
                    entry.3 = entry.3.min(row);
                    entry.4 = entry.4.max(row);
                } else {
                    bounds.push((name.into(), col, col, row, row));
                }
            }
        }
        let areas = bounds
            .into_iter()
            .map(|(name, min_col, max_col, min_row, max_row)| {
                (
                    name,
                    CellRegion {
                        col: min_col,
                        row: min_row,
                        col_span: max_col - min_col + 1,
                        row_span: max_row - min_row + 1,
                    },
                )
            })
            .collect();
        GridAreas { areas }
    }

    /// Resolve an area name to the explicit [`GridPlacement`] it covers. A
    /// creation-time helper called while authoring a child's placement; returns
    /// `None` for an unknown area.
    pub fn placement(&self, name: &str) -> Option<GridPlacement> {
        self.areas
            .iter()
            .find(|(candidate, _)| candidate.as_ref() == name)
            .map(|(_, region)| GridPlacement {
                column: Some(region.col),
                row: Some(region.row),
                column_span: region.col_span,
                row_span: region.row_span,
            })
    }
}

/// The warm, boxed per-grid-node payload: the two variable-length track
/// templates. Kept off the hot node arrays because only grid nodes carry it; an
/// ordinary node's column entry is `None`.
#[derive(Default)]
pub(crate) struct GridTracks {
    /// Explicit column template, folded: no [`TrackSizing::Length`] in it.
    pub columns: Vec<TrackSizing>,
    /// Explicit row template, folded.
    pub rows: Vec<TrackSizing>,
    /// The columns and rows as authored, when a track of them is a
    /// [`TrackSizing::Length`] the store folds into `columns` and `rows`.
    pub authored: Option<Box<[Vec<TrackSizing>; 2]>>,
    /// The column and row gaps' percentages (fractions) of the content box,
    /// added to the gaps' fixed extents where the box is known.
    pub gap_pct: [f32; 2],
}

impl GridTracks {
    /// The columns as authored.
    pub fn authored_columns(&self) -> &[TrackSizing] {
        self.authored.as_ref().map_or(&self.columns, |a| &a[0])
    }

    /// The rows as authored.
    pub fn authored_rows(&self) -> &[TrackSizing] {
        self.authored.as_ref().map_or(&self.rows, |a| &a[1])
    }
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

/// Place every child into the grid as CSS's sparse auto-placement does:
/// children pinned on both axes first (so the rest route around them), then
/// the row-pinned ones within their rows, then the column-pinned and the
/// unpinned ones row-major behind one cursor. Returns the number of rows used (>= 1). `occupied`
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

    // Row-pinned children next: each takes the first free block of its row past
    // the row-pinned children placed in that row before it. A row with no room
    // left takes it at its start, over what is there: the grid adds no columns.
    for (i, p) in placements.iter().enumerate() {
        let (None, Some(row)) = (p.column, p.row) else {
            continue;
        };
        let col_span = p.column_span.max(1).min(cols);
        let row_span = p.row_span.max(1);
        let mut col = placements[..i]
            .iter()
            .zip(&out[..i])
            .filter(|(q, _)| q.column.is_none() && q.row == Some(row))
            .map(|(_, region)| region.col + region.col_span)
            .max()
            .unwrap_or(0);
        while col + col_span <= cols && !block_free(occupied, cols, col, row, col_span, row_span) {
            col += 1;
        }
        if col + col_span > cols {
            col = 0;
        }
        mark_block(occupied, cols, col, row, col_span, row_span);
        out[i] = CellRegion {
            col,
            row,
            col_span,
            row_span,
        };
    }

    // The rest flow behind a row-major cursor: a column-pinned child takes the
    // first free block of its column at or past the cursor's row (the next row
    // when its column is behind the cursor), any other the first free block
    // that fits its span, creating implicit rows as needed.
    let mut cursor_col: u16 = 0;
    let mut cursor_row: u16 = 0;
    for (i, p) in placements.iter().enumerate() {
        if p.row.is_some() {
            continue;
        }
        let col_span = p.column_span.max(1).min(cols);
        let row_span = p.row_span.max(1);
        if let Some(c) = p.column {
            let col = c.min(cols - 1);
            let col_span = col_span.min(cols - col);
            if col < cursor_col {
                cursor_row += 1;
            }
            while !block_free(occupied, cols, col, cursor_row, col_span, row_span) {
                cursor_row += 1;
            }
            mark_block(occupied, cols, col, cursor_row, col_span, row_span);
            out[i] = CellRegion {
                col,
                row: cursor_row,
                col_span,
                row_span,
            };
            cursor_col = col + col_span;
            continue;
        }
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
/// supplies the content size for an `Auto`/`Minmax`/`FitContent` track (0.0
/// elsewhere). With `auto_mins`, each track's min-content size, `Auto` and
/// `FitContent` tracks whose content overflows the extent shrink toward it
/// (see [`shrink_to_fit`]). Writes `tracks.len()` extents into `out`
/// (cleared first).
pub(crate) fn solve_tracks(
    tracks: &[TrackSizing],
    gap: f32,
    content_extent: f32,
    auto_maxes: &[f32],
    auto_mins: Option<&[f32]>,
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
            TrackSizing::Relative { fixed, pct } => {
                let v = (fixed + content_extent * pct).max(0.0);
                out[i] = v;
                consumed += v;
            }
            TrackSizing::Length(terms) => {
                let v = (terms.dp + content_extent * terms.pct).max(0.0);
                out[i] = v;
                consumed += v;
            }
            TrackSizing::Auto => {
                let v = auto_maxes.get(i).copied().unwrap_or(0.0);
                out[i] = v;
                consumed += v;
            }
            TrackSizing::Minmax(lo, hi) => {
                // Clamp the content size into the band. `hi < lo` is a malformed
                // template; `clamp` would panic, so order the bounds first and
                // let `min` win (CSS treats an inverted max as ignored).
                let content = auto_maxes.get(i).copied().unwrap_or(0.0);
                let v = content.max(lo).min(hi.max(lo));
                out[i] = v;
                consumed += v;
            }
            TrackSizing::FitContent(limit) => {
                let content = auto_maxes.get(i).copied().unwrap_or(0.0);
                let v = content.min(limit.max(0.0)).max(0.0);
                out[i] = v;
                consumed += v;
            }
            TrackSizing::Fr(w) => fr_total += w.max(0.0),
            TrackSizing::FlexMin(min, fr) => {
                // A flexible track with a pixel floor (`minmax(min, <fr>fr)`): the
                // `min` is consumed up front (like a fixed base) and the `fr`
                // weight joins the sweep, so pass 2 adds its leftover share on top
                // of the base already written here.
                let base = min.max(0.0);
                out[i] = base;
                consumed += base;
                fr_total += fr.max(0.0);
            }
        }
    }

    if let Some(mins) = auto_mins
        && consumed + gaps_total > content_extent
    {
        consumed = shrink_to_fit(
            tracks,
            auto_maxes,
            mins,
            content_extent - gaps_total,
            consumed,
            out,
        );
    }

    // Pass 2: distribute the remaining free space across the flexible tracks
    // (`Fr` and the `fr` part of `FlexMin`) with a re-normalizing sweep. A
    // `FlexMin` track keeps its `min` base already in `out` and only adds its
    // leftover share, so a track that gets no share still holds at `min`.
    if fr_total > 0.0 {
        let mut remaining_free = (content_extent - consumed - gaps_total).max(0.0);
        let mut remaining_fr = fr_total;
        for (i, t) in tracks.iter().enumerate() {
            let (w, add_to_base) = match *t {
                TrackSizing::Fr(w) => (w.max(0.0), false),
                TrackSizing::FlexMin(_, fr) => (fr.max(0.0), true),
                _ => continue,
            };
            let size = if remaining_fr > 0.0 {
                remaining_free * (w / remaining_fr)
            } else {
                0.0
            };
            if add_to_base {
                out[i] += size;
            } else {
                out[i] = size;
            }
            remaining_free -= size;
            remaining_fr -= w;
        }
    }
}

/// The extent a track of `track` takes at its cells' max-content extent
/// `content` where no box is known: a fixed track its value, a percentage
/// nothing, a flexible one its content (at least its floor).
pub(crate) fn content_track(track: TrackSizing, content: f32) -> f32 {
    match track {
        TrackSizing::Fixed(v) => v,
        TrackSizing::Percent(_) => 0.0,
        TrackSizing::Relative { fixed, .. } => fixed.max(0.0),
        TrackSizing::Length(terms) => terms.dp.max(0.0),
        TrackSizing::Auto | TrackSizing::Fr(_) => content,
        TrackSizing::Minmax(lo, hi) => content.max(lo).min(hi.max(lo)),
        TrackSizing::FitContent(limit) => content.min(limit.max(0.0)).max(0.0),
        TrackSizing::FlexMin(min, _) => content.max(min),
    }
}

/// The narrowest a track of `track` takes over cells whose min-content width
/// is `content`: a fixed track its value, a band its lower bound, a flexible
/// one its floor, a content-sized one its cells' min-content.
pub(crate) fn min_content_track(track: TrackSizing, content: f32) -> f32 {
    match track {
        TrackSizing::Fixed(v) => v,
        TrackSizing::Percent(_) => 0.0,
        TrackSizing::Relative { fixed, .. } => fixed.max(0.0),
        TrackSizing::Length(terms) => terms.dp.max(0.0),
        TrackSizing::Minmax(lo, _) | TrackSizing::FlexMin(lo, _) => lo.max(0.0),
        TrackSizing::FitContent(limit) => content.min(limit.max(0.0)).max(0.0),
        TrackSizing::Auto | TrackSizing::Fr(_) => content,
    }
}

/// Whether a track's content sizes it from its cells' min-content up to their
/// max-content: `Auto`, and `FitContent` below its limit.
fn shrinks_to_content(track: TrackSizing) -> bool {
    matches!(track, TrackSizing::Auto | TrackSizing::FitContent(..))
}

/// Shrinks the content-sized tracks of `tracks`, resolved in `out` to their
/// content `maxes` and to `consumed` in all, so they fit `room`, as CSS's
/// track sizing does: each starts at its min-content size `mins[i]` (never
/// above its resolved size), and what the room leaves past the other tracks
/// and those minimums is shared equally among them, none growing past its
/// resolved size. Returns what the tracks consume after.
fn shrink_to_fit(
    tracks: &[TrackSizing],
    maxes: &[f32],
    mins: &[f32],
    room: f32,
    consumed: f32,
    out: &mut [f32],
) -> f32 {
    let cap = |i: usize| match tracks[i] {
        TrackSizing::Auto => maxes.get(i).copied().unwrap_or(0.0),
        TrackSizing::FitContent(limit) => maxes
            .get(i)
            .copied()
            .unwrap_or(0.0)
            .min(limit.max(0.0))
            .max(0.0),
        _ => 0.0,
    };
    let base = |i: usize| mins.get(i).copied().unwrap_or(0.0).min(cap(i));
    let shrinking = || (0..tracks.len()).filter(|&i| shrinks_to_content(tracks[i]));
    let caps: f32 = shrinking().map(cap).sum();
    let bases: f32 = shrinking().map(base).sum();
    let others = consumed - caps;
    let mut free = room - others - bases;
    if free >= caps - bases {
        return consumed;
    }
    for i in shrinking() {
        out[i] = base(i);
    }
    // Share the free room equally, freezing each track at its cap: every
    // round spends it all or freezes one more track.
    let open = |i: usize, out: &[f32]| cap(i) - out[i] > f32::EPSILON;
    while free > f32::EPSILON {
        let count = shrinking().filter(|&i| open(i, out)).count();
        if count == 0 {
            break;
        }
        let share = free / count as f32;
        for i in shrinking() {
            if open(i, out) {
                let add = share.min(cap(i) - out[i]);
                out[i] += add;
                free -= add;
            }
        }
    }
    others + shrinking().map(|i| out[i]).sum::<f32>()
}

/// Whether a track grows to fit item content — the intrinsically-sized tracks
/// (`Auto`, `Minmax`, `FitContent`) that read the `auto_maxes` channel. `Fixed`,
/// `Percent`, `Fr`, and `FlexMin` do not: a spanning item's excess is never
/// attributed to them (they are, respectively, exact, extent-relative, a
/// free-space share, and a floored free-space share).
fn track_grows_to_content(track: TrackSizing) -> bool {
    matches!(
        track,
        TrackSizing::Auto | TrackSizing::Minmax(..) | TrackSizing::FitContent(..)
    )
}

/// The extent a track already accounts for when apportioning a spanning item's
/// content, evaluated *before* the `Fr` sweep. `Fixed` is its value; `Percent` a
/// fraction of the content extent; a content-sized track its current span-1
/// baseline in `auto` (0 if none); `Fr` contributes 0 pre-solve (its share is the
/// leftover, not a content need); `FlexMin` contributes its `min` floor (the base
/// consumed before the sweep). Used to compute how much of a spanning item's size
/// the covered tracks do not yet cover.
fn track_prebase(track: TrackSizing, content_extent: f32, auto: f32) -> f32 {
    match track {
        TrackSizing::Fixed(v) => v,
        TrackSizing::Percent(frac) => content_extent * frac,
        TrackSizing::Relative { fixed, pct } => (fixed + content_extent * pct).max(0.0),
        TrackSizing::Length(terms) => (terms.dp + content_extent * terms.pct).max(0.0),
        TrackSizing::Auto | TrackSizing::Minmax(..) | TrackSizing::FitContent(..) => auto,
        TrackSizing::Fr(_) => 0.0,
        TrackSizing::FlexMin(min, _) => min.max(0.0),
    }
}

/// Refine the `auto_maxes` channel for the tracks along one axis with the
/// contribution of items that span more than one track (CSS Grid "distribute
/// extra space to spanned tracks", §11.5). Run *after* the span-1 baselines are
/// in `auto_maxes`: for each spanning item, the space its content still needs
/// beyond what the covered tracks already account for
/// (`measured − Σ track_prebase − interior gaps`, floored at 0) is spread equally
/// across the growable tracks it covers and `max`-ed into each. Fixed/Percent/Fr
/// tracks absorb none of it. A span with no growable track contributes nothing
/// (its content is honored by the cell rect, not by growing a fixed track).
///
/// `spans` yields `(start, span, measured)` for every item on this axis;
/// callers pass the item's covered track range and its natural main size.
pub(crate) fn distribute_spanning_auto(
    tracks: &[TrackSizing],
    gap: f32,
    content_extent: f32,
    spans: impl Iterator<Item = (u16, u16, f32)>,
    auto_maxes: &mut [f32],
) {
    for (start, span, measured) in spans {
        if span <= 1 {
            continue;
        }
        let s = start as usize;
        let end = (s + span as usize).min(tracks.len());
        if s >= end {
            continue;
        }
        let mut covered_base = 0.0f32;
        let mut growable = 0u32;
        for i in s..end {
            covered_base += track_prebase(tracks[i], content_extent, auto_maxes[i]);
            if track_grows_to_content(tracks[i]) {
                growable += 1;
            }
        }
        if growable == 0 {
            continue;
        }
        let interior_gaps = gap * (span.saturating_sub(1) as f32);
        let excess = (measured - covered_base - interior_gaps).max(0.0);
        if excess <= 0.0 {
            continue;
        }
        let share = excess / growable as f32;
        for i in s..end {
            if track_grows_to_content(tracks[i]) {
                auto_maxes[i] = auto_maxes[i].max(share);
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
    fn a_child_pinned_on_one_axis_flows_along_the_other() {
        let pinned = |column, row| GridPlacement {
            column,
            row,
            ..auto(1, 1)
        };
        // 3 columns. Row-pinned children fill row 1 left to right; a
        // column-pinned one takes the first free row of column 2; an
        // unpinned one flows behind it, never back before the cursor; a
        // column behind the cursor moves to the next row.
        let placements = [
            pinned(None, Some(1)),
            pinned(Some(2), None),
            pinned(None, Some(1)),
            auto(1, 1),
            pinned(Some(0), None),
        ];
        let (mut occ, mut out) = (Vec::new(), Vec::new());
        let rows = place_children(3, &placements, &mut occ, &mut out);
        let at: Vec<(u16, u16)> = out.iter().map(|r| (r.col, r.row)).collect();
        assert_eq!(at, [(0, 1), (2, 0), (1, 1), (2, 1), (0, 2)]);
        assert_eq!(rows, 3);
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
    fn auto_tracks_too_wide_for_the_extent_shrink_toward_their_min_content() {
        let tracks = [
            TrackSizing::Auto,
            TrackSizing::Auto,
            TrackSizing::Fixed(20.0),
        ];
        let mut out = Vec::new();
        // 300 + 100 of content in 200 less the fixed 20 and two 10 gaps: each
        // starts at its min (50, 40) and the 70 left is shared equally.
        solve_tracks(
            &tracks,
            10.0,
            200.0,
            &[300.0, 100.0, 0.0],
            Some(&[50.0, 40.0, 0.0]),
            &mut out,
        );
        assert_eq!(out, vec![85.0, 75.0, 20.0]);
        // A track reaching its max content stops; the rest goes to the other.
        solve_tracks(
            &tracks,
            10.0,
            200.0,
            &[300.0, 60.0, 0.0],
            Some(&[50.0, 40.0, 0.0]),
            &mut out,
        );
        assert_eq!(out, vec![100.0, 60.0, 20.0]);
        // Too narrow for even the mins: each holds its min and overflows.
        solve_tracks(
            &tracks,
            10.0,
            60.0,
            &[300.0, 100.0, 0.0],
            Some(&[50.0, 40.0, 0.0]),
            &mut out,
        );
        assert_eq!(out, vec![50.0, 40.0, 20.0]);
        // Room for all: max content, as without mins.
        solve_tracks(
            &tracks,
            10.0,
            600.0,
            &[300.0, 100.0, 0.0],
            Some(&[50.0, 40.0, 0.0]),
            &mut out,
        );
        assert_eq!(out, vec![300.0, 100.0, 20.0]);
    }

    #[test]
    fn fixed_tracks_take_their_value() {
        let mut out = Vec::new();
        solve_tracks(
            &[TrackSizing::Fixed(50.0), TrackSizing::Fixed(80.0)],
            0.0,
            1000.0,
            &[0.0, 0.0],
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
            &mut out,
        );
        assert_eq!(out, vec![100.0, 100.0]);
    }

    #[test]
    fn minmax_clamps_content_below_min_up_to_min() {
        // Content 20 < min 50 → the track floors at 50.
        let mut out = Vec::new();
        solve_tracks(
            &[TrackSizing::Minmax(50.0, 200.0)],
            0.0,
            1000.0,
            &[20.0],
            None,
            &mut out,
        );
        assert_eq!(out, vec![50.0]);
    }

    #[test]
    fn minmax_passes_content_inside_the_band_through() {
        // Content 120 is within [50, 200] → taken as-is.
        let mut out = Vec::new();
        solve_tracks(
            &[TrackSizing::Minmax(50.0, 200.0)],
            0.0,
            1000.0,
            &[120.0],
            None,
            &mut out,
        );
        assert_eq!(out, vec![120.0]);
    }

    #[test]
    fn minmax_clamps_content_above_max_down_to_max() {
        // Content 350 > max 200 → the track caps at 200.
        let mut out = Vec::new();
        solve_tracks(
            &[TrackSizing::Minmax(50.0, 200.0)],
            0.0,
            1000.0,
            &[350.0],
            None,
            &mut out,
        );
        assert_eq!(out, vec![200.0]);
    }

    #[test]
    fn minmax_with_inverted_bounds_lets_min_win() {
        // A malformed band (max < min): min is authoritative, no panic.
        let mut out = Vec::new();
        solve_tracks(
            &[TrackSizing::Minmax(120.0, 40.0)],
            0.0,
            1000.0,
            &[500.0],
            None,
            &mut out,
        );
        assert_eq!(out, vec![120.0]);
    }

    #[test]
    fn fit_content_caps_content_at_the_limit() {
        // Content 90 under the 150 limit passes; content 300 over it caps at 150.
        let mut out = Vec::new();
        solve_tracks(
            &[
                TrackSizing::FitContent(150.0),
                TrackSizing::FitContent(150.0),
            ],
            0.0,
            1000.0,
            &[90.0, 300.0],
            None,
            &mut out,
        );
        assert_eq!(out, vec![90.0, 150.0]);
    }

    #[test]
    fn minmax_is_a_fixed_track_the_fr_sweep_splits_the_remainder() {
        // [Minmax(50,200) content 300 → 200, Fr(1)] over 500px, no gap → 200,
        // then 300 free to the single Fr track.
        let mut out = Vec::new();
        solve_tracks(
            &[TrackSizing::Minmax(50.0, 200.0), TrackSizing::Fr(1.0)],
            0.0,
            500.0,
            &[300.0, 0.0],
            None,
            &mut out,
        );
        assert_eq!(out, vec![200.0, 300.0]);
    }

    #[test]
    fn repeat_expands_to_a_hand_written_template() {
        // repeat(3, Fr(1)) is byte-for-byte three Fr(1) tracks.
        assert_eq!(
            repeated(3, TrackSizing::Fr(1.0)),
            vec![
                TrackSizing::Fr(1.0),
                TrackSizing::Fr(1.0),
                TrackSizing::Fr(1.0)
            ]
        );
        // Appending onto an existing template keeps the leading tracks.
        let mut cols = vec![TrackSizing::Fixed(80.0)];
        repeat(2, TrackSizing::Auto, &mut cols);
        assert_eq!(
            cols,
            vec![
                TrackSizing::Fixed(80.0),
                TrackSizing::Auto,
                TrackSizing::Auto
            ]
        );
        // A zero count appends nothing.
        let mut empty = Vec::new();
        repeat(0, TrackSizing::Fr(1.0), &mut empty);
        assert!(empty.is_empty());
    }

    #[test]
    fn a_span_2_item_splits_its_content_across_two_auto_columns() {
        // Two Auto columns, no span-1 baseline. A span-2 item measuring 300 wide,
        // no gap, spreads 300/2 = 150 into each Auto track.
        let tracks = [TrackSizing::Auto, TrackSizing::Auto];
        let mut auto = vec![0.0f32; 2];
        distribute_spanning_auto(
            &tracks,
            0.0,
            1000.0,
            [(0u16, 2u16, 300.0)].into_iter(),
            &mut auto,
        );
        assert_eq!(auto, vec![150.0, 150.0]);
    }

    #[test]
    fn spanning_excess_is_measured_over_the_span_after_subtracting_a_fixed_track() {
        // [Fixed(100), Auto]: a span-2 item of 260 covers the fixed 100, so only
        // 160 is attributed — all of it to the single growable Auto track.
        let tracks = [TrackSizing::Fixed(100.0), TrackSizing::Auto];
        let mut auto = vec![0.0f32; 2];
        distribute_spanning_auto(
            &tracks,
            0.0,
            1000.0,
            [(0u16, 2u16, 260.0)].into_iter(),
            &mut auto,
        );
        assert_eq!(auto, vec![0.0, 160.0]);
    }

    #[test]
    fn a_span_item_narrower_than_its_tracks_baseline_adds_nothing() {
        // Auto tracks already at 200 each from span-1 items; a span-2 item of 150
        // needs less than the 400 the tracks already provide → no growth.
        let tracks = [TrackSizing::Auto, TrackSizing::Auto];
        let mut auto = vec![200.0f32, 200.0];
        distribute_spanning_auto(
            &tracks,
            0.0,
            1000.0,
            [(0u16, 2u16, 150.0)].into_iter(),
            &mut auto,
        );
        assert_eq!(auto, vec![200.0, 200.0]);
    }

    #[test]
    fn interior_gaps_are_subtracted_before_distributing_across_the_span() {
        // Two Auto columns with a 20px gap: a span-2 item of 220 spends 20 on the
        // interior gap, leaving 200 split as 100 into each track.
        let tracks = [TrackSizing::Auto, TrackSizing::Auto];
        let mut auto = vec![0.0f32; 2];
        distribute_spanning_auto(
            &tracks,
            20.0,
            1000.0,
            [(0u16, 2u16, 220.0)].into_iter(),
            &mut auto,
        );
        assert_eq!(auto, vec![100.0, 100.0]);
    }

    #[test]
    fn a_span_over_only_fixed_and_fr_tracks_grows_nothing() {
        // No growable track in the span (Fixed + Fr) → the item's content is left
        // to the cell rect, no auto growth attributed.
        let tracks = [TrackSizing::Fixed(50.0), TrackSizing::Fr(1.0)];
        let mut auto = vec![0.0f32; 2];
        distribute_spanning_auto(
            &tracks,
            0.0,
            1000.0,
            [(0u16, 2u16, 400.0)].into_iter(),
            &mut auto,
        );
        assert_eq!(auto, vec![0.0, 0.0]);
    }

    #[test]
    fn span_1_items_are_ignored_by_the_spanning_distribution() {
        // The span-1 pass owns single-track items; distribute_spanning_auto must
        // leave them untouched even if handed one.
        let tracks = [TrackSizing::Auto, TrackSizing::Auto];
        let mut auto = vec![0.0f32; 2];
        distribute_spanning_auto(
            &tracks,
            0.0,
            1000.0,
            [(0u16, 1u16, 300.0)].into_iter(),
            &mut auto,
        );
        assert_eq!(auto, vec![0.0, 0.0]);
    }

    #[test]
    fn resolve_line_maps_a_known_name_to_its_index_and_none_otherwise() {
        let names: LineNames = vec![("nav-start".into(), 0), ("nav-end".into(), 2)];
        assert_eq!(resolve_line(&names, "nav-start"), Some(0));
        assert_eq!(resolve_line(&names, "nav-end"), Some(2));
        assert_eq!(resolve_line(&names, "missing"), None);
    }

    #[test]
    fn grid_areas_from_rows_bounds_each_name_and_spans_across_cells() {
        // [["nav", "nav"], ["side", "main"]]: nav spans both columns on row 0;
        // side and main are single cells on row 1.
        let areas = GridAreas::from_rows([["nav", "nav"], ["side", "main"]]);
        assert_eq!(
            areas.placement("nav"),
            Some(GridPlacement {
                column: Some(0),
                row: Some(0),
                column_span: 2,
                row_span: 1,
            })
        );
        assert_eq!(
            areas.placement("side"),
            Some(GridPlacement {
                column: Some(0),
                row: Some(1),
                column_span: 1,
                row_span: 1,
            })
        );
        assert_eq!(
            areas.placement("main"),
            Some(GridPlacement {
                column: Some(1),
                row: Some(1),
                column_span: 1,
                row_span: 1,
            })
        );
        assert_eq!(areas.placement("unknown"), None);
    }

    #[test]
    fn grid_areas_span_a_name_across_both_rows_and_columns() {
        // A 2x2 block where "main" fills the bottom-right 2x1 and "side" the whole
        // left column: exercises row spanning and column spanning at once.
        let areas = GridAreas::from_rows([["side", "head", "head"], ["side", "main", "main"]]);
        assert_eq!(
            areas.placement("side"),
            Some(GridPlacement {
                column: Some(0),
                row: Some(0),
                column_span: 1,
                row_span: 2,
            })
        );
        assert_eq!(
            areas.placement("head"),
            Some(GridPlacement {
                column: Some(1),
                row: Some(0),
                column_span: 2,
                row_span: 1,
            })
        );
        assert_eq!(
            areas.placement("main"),
            Some(GridPlacement {
                column: Some(1),
                row: Some(1),
                column_span: 2,
                row_span: 1,
            })
        );
    }

    #[test]
    fn grid_areas_ignores_the_empty_dot_cell() {
        // "." is the intentionally-empty cell and is never registered as an area.
        let areas = GridAreas::from_rows([["a", "."], [".", "b"]]);
        assert_eq!(areas.placement("."), None);
        assert_eq!(
            areas.placement("a"),
            Some(GridPlacement {
                column: Some(0),
                row: Some(0),
                column_span: 1,
                row_span: 1,
            })
        );
        assert_eq!(
            areas.placement("b"),
            Some(GridPlacement {
                column: Some(1),
                row: Some(1),
                column_span: 1,
                row_span: 1,
            })
        );
    }

    #[test]
    fn flexmin_single_track_takes_min_then_all_leftover() {
        // A lone `minmax(200, 1fr)` over 500px, no gap: min=200 consumed, the whole
        // 300 leftover is its Fr share → 200 + 300 = 500. Content channel unused
        // (a FlexMin does not read auto_maxes).
        let mut out = Vec::new();
        solve_tracks(
            &[TrackSizing::FlexMin(200.0, 1.0)],
            0.0,
            500.0,
            &[0.0],
            None,
            &mut out,
        );
        assert_eq!(out, vec![500.0]);
    }

    #[test]
    fn flexmin_multiple_tracks_split_leftover_after_each_min() {
        // Three `minmax(100, 1fr)` over 600px, no gap: 3×100 = 300 consumed, 300
        // leftover split equally by weight → each 100 + 100 = 200.
        let mut out = Vec::new();
        solve_tracks(
            &[
                TrackSizing::FlexMin(100.0, 1.0),
                TrackSizing::FlexMin(100.0, 1.0),
                TrackSizing::FlexMin(100.0, 1.0),
            ],
            0.0,
            600.0,
            &[0.0, 0.0, 0.0],
            None,
            &mut out,
        );
        assert_eq!(out, vec![200.0, 200.0, 200.0]);
    }

    #[test]
    fn flexmin_holds_at_min_when_no_leftover() {
        // Two `minmax(200, 1fr)` over 300px: 2×200 = 400 already exceeds the extent,
        // so leftover is floored at 0 and each track holds at its 200 min (the row
        // overflows — a solved track never shrinks below its floor).
        let mut out = Vec::new();
        solve_tracks(
            &[
                TrackSizing::FlexMin(200.0, 1.0),
                TrackSizing::FlexMin(200.0, 1.0),
            ],
            0.0,
            300.0,
            &[0.0, 0.0],
            None,
            &mut out,
        );
        assert_eq!(out, vec![200.0, 200.0]);
    }

    #[test]
    fn flexmin_and_fr_share_the_sweep_by_weight() {
        // `minmax(100, 1fr)` beside a bare `Fr(1)` over 500px: the FlexMin takes its
        // 100 min first (consumed), then the 400 leftover splits by equal weight →
        // FlexMin 100 + 200 = 300, Fr 200.
        let mut out = Vec::new();
        solve_tracks(
            &[TrackSizing::FlexMin(100.0, 1.0), TrackSizing::Fr(1.0)],
            0.0,
            500.0,
            &[0.0, 0.0],
            None,
            &mut out,
        );
        assert_eq!(out, vec![300.0, 200.0]);
    }

    #[test]
    fn flexmin_min_gets_gaps_subtracted_from_the_leftover() {
        // Two `minmax(100, 1fr)` over 500px with a 20px gap: 2×100 consumed + 20 gap
        // → 280 leftover, split → each 100 + 140 = 240.
        let mut out = Vec::new();
        solve_tracks(
            &[
                TrackSizing::FlexMin(100.0, 1.0),
                TrackSizing::FlexMin(100.0, 1.0),
            ],
            20.0,
            500.0,
            &[0.0, 0.0],
            None,
            &mut out,
        );
        assert_eq!(out, vec![240.0, 240.0]);
    }

    #[test]
    fn adaptive_columns_constructors_set_the_mode() {
        let fill = AdaptiveColumns::auto_fill(200.0, TrackMax::Fr(1.0));
        assert_eq!(fill.mode, AutoRepeat::Fill);
        assert_eq!(fill.min, 200.0);
        assert_eq!(fill.max, TrackMax::Fr(1.0));
        let fit = AdaptiveColumns::auto_fit(150.0, TrackMax::Px(300.0));
        assert_eq!(fit.mode, AutoRepeat::Fit);
        assert_eq!(fit.min, 150.0);
        assert_eq!(fit.max, TrackMax::Px(300.0));
    }
}
