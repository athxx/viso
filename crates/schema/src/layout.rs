//! The layout vocabulary: axes, lengths, alignment, insets and grid tracks.

/// The main axis a Flex container lays its children along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Children flow left-to-right; main = x, cross = y.
    Row,
    /// Children flow top-to-bottom; main = y, cross = x.
    Column,
}

/// How a single length resolves against its container.
///
/// Every extent is in logical pixels (dp). A `Fixed` length is an exact dp
/// size. A `Relative` length is `fixed + pct × basis`, where the basis is the
/// parent's content box on the same axis — `50%` is a ratio of that box, not a
/// share of what siblings leave over. A `Fill` length claims a share of the
/// leftover space along the main axis, split between siblings by `weight`. A
/// `Fit` length takes the node's content extent within the room its parent
/// gives it, never narrower than its `MinContent` width; `MinContent` and
/// `MaxContent` take the narrowest and the widest the content lays out in,
/// whatever the room.
///
/// The measure pass runs before any basis is known, so a `Relative` length
/// measures like `Fit` (a percentage of an indefinite size is its content) and
/// resolves against the basis only when the parent places it. A resolved
/// `Relative` extent is never negative.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    /// An exact extent in dp.
    Fixed(f32),
    /// `fixed + pct × basis`, with `pct` a fraction of the basis (`0.5` is `50%`).
    Relative { fixed: f32, pct: f32 },
    /// A share of leftover main-axis space, proportional to `weight`.
    Fill { weight: f32 },
    /// The content's extent, as wide as the room allows down to its
    /// `MinContent` width.
    Fit,
    /// The narrowest width the content lays out in without overflowing — a
    /// text's widest stretch no line breaks inside — and the height it takes
    /// at it.
    MinContent,
    /// The content's extent laid out unconstrained: a text on one line per
    /// hard break.
    MaxContent,
}

impl Length {
    /// A unit-weight fill (the common "take the rest" case).
    pub const fn fill() -> Self {
        Length::Fill { weight: 1.0 }
    }

    /// An exact extent in dp.
    pub const fn dp(v: f32) -> Self {
        Length::Fixed(v)
    }

    /// A percentage of the basis: `Length::pct(50.0)` is `50%`.
    pub const fn pct(percent: f32) -> Self {
        Length::Relative {
            fixed: 0.0,
            pct: percent / 100.0,
        }
    }

    /// The extent this length takes against `basis`, or `None` for one the
    /// container or the content decides.
    #[inline]
    pub fn resolve(self, basis: f32) -> Option<f32> {
        match self {
            Length::Fixed(v) => Some(v),
            Length::Relative { fixed, pct } => Some((fixed + pct * basis).max(0.0)),
            Length::Fill { .. } | Length::Fit | Length::MinContent | Length::MaxContent => None,
        }
    }
}

/// Cross-axis placement of children within a container.
///
/// `Start`/`Center`/`End` position a child of its natural cross size at the
/// near edge, middle, or far edge of the container's cross extent. `Stretch`
/// grows the child to fill the cross extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// Pin to the near cross edge (top for Row, left for Column).
    Start,
    /// Center within the cross extent.
    Center,
    /// Pin to the far cross edge.
    End,
    /// Grow to fill the cross extent.
    Stretch,
}

/// Main-axis placement of the children as a group within a container.
///
/// [`Align`] handles the cross axis (each child within the line); `Justify`
/// handles the main axis (the whole packed group within the container's main
/// extent). It only has a visible effect when the children leave main-axis
/// slack — i.e. none of them is [`Length::Fill`], since a Fill child consumes
/// the free space and leaves nothing to distribute (the same relationship
/// [`Align::Center`] has with [`Align::Stretch`]). `Center`/`End` shift the
/// group by the leftover free space; `Start` (the default) pins it to the near
/// edge, preserving the historical near-edge packing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Justify {
    /// Pack the group at the near main edge (left for Row, top for Column).
    #[default]
    Start,
    /// Center the group within the container's main extent.
    Center,
    /// Pack the group at the far main edge.
    End,
}

/// Four-edge inset in pixels (padding for a container).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Inset {
    /// Left edge.
    pub left: f32,
    /// Top edge.
    pub top: f32,
    /// Right edge.
    pub right: f32,
    /// Bottom edge.
    pub bottom: f32,
}

impl Inset {
    /// A uniform inset on all four edges.
    pub const fn all(v: f32) -> Self {
        Inset {
            left: v,
            top: v,
            right: v,
            bottom: v,
        }
    }

    /// Total inset along the main axis (both edges).
    #[inline]
    pub fn main(self, axis: Axis) -> f32 {
        match axis {
            Axis::Row => self.left + self.right,
            Axis::Column => self.top + self.bottom,
        }
    }

    /// Total inset across the cross axis (both edges).
    #[inline]
    pub fn cross(self, axis: Axis) -> f32 {
        match axis {
            Axis::Row => self.top + self.bottom,
            Axis::Column => self.left + self.right,
        }
    }

    /// Near-edge inset along the main axis (left for Row, top for Column).
    #[inline]
    pub fn main_start(self, axis: Axis) -> f32 {
        match axis {
            Axis::Row => self.left,
            Axis::Column => self.top,
        }
    }

    /// Near-edge inset across the cross axis (top for Row, left for Column).
    #[inline]
    pub fn cross_start(self, axis: Axis) -> f32 {
        match axis {
            Axis::Row => self.top,
            Axis::Column => self.left,
        }
    }
}

impl core::ops::Add for Inset {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self {
        Inset {
            left: self.left + rhs.left,
            top: self.top + rhs.top,
            right: self.right + rhs.right,
            bottom: self.bottom + rhs.bottom,
        }
    }
}

/// A box rule's length: `fixed` dp plus `pct` of its percent basis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    /// The dp part.
    pub fixed: f32,
    /// The fraction of the basis (`0.5` is `50%`).
    pub pct: f32,
}

impl Span {
    /// No length.
    pub const ZERO: Self = Self::dp(0.0);
    /// No bound: a maximum that never clamps.
    pub const UNBOUNDED: Self = Self::dp(f32::INFINITY);

    /// `v` dp.
    pub const fn dp(v: f32) -> Self {
        Self { fixed: v, pct: 0.0 }
    }

    /// A percentage of the basis: `Span::pct(50.0)` is `50%`.
    pub const fn pct(percent: f32) -> Self {
        Self {
            fixed: 0.0,
            pct: percent / 100.0,
        }
    }

    /// The extent on `basis`: the sum on a definite one; on an indefinite one
    /// the dp part, or `fallback` for a span that is all percentage.
    #[inline]
    pub fn resolve(self, basis: f32, definite: bool, fallback: f32) -> f32 {
        if self.pct == 0.0 {
            self.fixed
        } else if definite {
            self.fixed + self.pct * basis
        } else if self.fixed == 0.0 {
            fallback
        } else {
            self.fixed
        }
    }
}

/// Four physical edges of [`Span`]s.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpanInset {
    /// Left edge.
    pub left: Span,
    /// Top edge.
    pub top: Span,
    /// Right edge.
    pub right: Span,
    /// Bottom edge.
    pub bottom: Span,
}

impl SpanInset {
    /// No inset.
    pub const ZERO: Self = Self {
        left: Span::ZERO,
        top: Span::ZERO,
        right: Span::ZERO,
        bottom: Span::ZERO,
    };

    /// The inset on a box of `width` × `height`, each definite or not: the
    /// left and right edges read the width, the top and bottom the height.
    #[inline]
    pub fn resolve(self, width: f32, height: f32, definite: [bool; 2]) -> Inset {
        Inset {
            left: self.left.resolve(width, definite[0], 0.0),
            top: self.top.resolve(height, definite[1], 0.0),
            right: self.right.resolve(width, definite[0], 0.0),
            bottom: self.bottom.resolve(height, definite[1], 0.0),
        }
    }

    /// Whether an edge reads its basis.
    pub fn has_pct(self) -> bool {
        [self.left, self.top, self.right, self.bottom]
            .iter()
            .any(|span| span.pct != 0.0)
    }
}

/// What bounds a node's box beyond its size request, and the room it keeps
/// around itself: the extents it may not shrink below or grow past (a minimum
/// wins over a smaller maximum), its margin, and the percentage part of a
/// container's padding. A size bound and a margin read the parent's content
/// box, the width for the horizontal ones and the height for the vertical
/// ones, as does the padding percentage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxRules {
    /// The least width.
    pub min_width: Span,
    /// The greatest width.
    pub max_width: Span,
    /// The least height.
    pub min_height: Span,
    /// The greatest height.
    pub max_height: Span,
    /// Room outside the box, on each edge; may be negative.
    pub margin: SpanInset,
    /// The padding each edge adds as a fraction of the parent's content box.
    pub padding_pct: Inset,
}

impl BoxRules {
    /// No rule.
    pub const NONE: Self = Self {
        min_width: Span::ZERO,
        max_width: Span::UNBOUNDED,
        min_height: Span::ZERO,
        max_height: Span::UNBOUNDED,
        margin: SpanInset::ZERO,
        padding_pct: Inset {
            left: 0.0,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        },
    };

    /// The least and greatest extent on `axis` over a basis of `basis`.
    #[inline]
    pub fn bounds(&self, axis: Axis, basis: f32, definite: bool) -> (f32, f32) {
        let (min, max) = match axis {
            Axis::Row => (self.min_width, self.max_width),
            Axis::Column => (self.min_height, self.max_height),
        };
        (
            min.resolve(basis, definite, 0.0).max(0.0),
            max.resolve(basis, definite, f32::INFINITY).max(0.0),
        )
    }

    /// `extent` on `axis` held within the bounds; the minimum wins.
    #[inline]
    pub fn clamp(&self, axis: Axis, extent: f32, basis: f32, definite: bool) -> f32 {
        let (min, max) = self.bounds(axis, basis, definite);
        extent.min(max).max(min)
    }

    /// Whether the bounds on `axis` can move an extent.
    #[inline]
    pub fn bounds_on(&self, axis: Axis) -> bool {
        match axis {
            Axis::Row => self.min_width != Span::ZERO || self.max_width != Span::UNBOUNDED,
            Axis::Column => self.min_height != Span::ZERO || self.max_height != Span::UNBOUNDED,
        }
    }
}

impl Default for BoxRules {
    fn default() -> Self {
        Self::NONE
    }
}

/// How a grid cell's content is aligned within the cell along the block (row /
/// vertical) axis. `Stretch` (the default) grows a fillable child to the cell
/// height; the others place the child at its natural height and shift it.
/// `Baseline` aligns each cell's first-line text baseline to the tallest
/// baseline in its row, so mixed-size labels sit on a common baseline; a cell
/// with no text baseline falls back to `Start`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum AlignItems {
    /// Grow a fillable child to the full cell height (CSS default).
    #[default]
    Stretch,
    /// Pin the child to the top of the cell at its natural height.
    Start,
    /// Center the child vertically at its natural height.
    Center,
    /// Pin the child to the bottom of the cell at its natural height.
    End,
    /// Align the child's first-line baseline to its row's tallest baseline.
    Baseline,
}

/// How one grid track (a column or a row) is sized.
///
/// `Fixed` is an exact pixel extent. `Percent` is a fraction (0.0..=1.0) of the
/// grid's content extent along that track's axis. `Auto` sizes to the largest
/// natural main size among the single-track items on the track. `Fr` claims a
/// share of the free space left after the fixed/percent/auto tracks, resolved by
/// a re-normalizing sweep so a clamp on one flexible track cascades to the rest.
/// `Minmax` clamps the track's content size into a `[min, max]` pixel band;
/// `FitContent` caps the content size at a pixel limit. Both resolve like a
/// content-sized track (they read the same natural-max channel as `Auto`) and
/// are fixed-size tracks — they consume space before the `Fr` sweep, they never
/// take a share of the free space.
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
    /// Content size clamped to a `[min, max]` pixel band. A fixed-size track: it
    /// resolves to `content.clamp(min, max)` and does not enter the `Fr` sweep.
    Minmax(f32, f32),
    /// Content size capped at a pixel limit: `content.min(limit)`. A fixed-size
    /// track, resolved like `Auto` but bounded above.
    FitContent(f32),
    /// A flexible track with a pixel floor: `minmax(min, <fr>fr)`. Unlike the
    /// fixed-size `Minmax`, its upper bound is a free-space share, so it takes
    /// `min` first (counted as consumed) and then grows by an `Fr(fr)` share of
    /// the leftover — the CSS `minmax(min, 1fr)` semantics behind the responsive
    /// `repeat(auto-fill/auto-fit, minmax(min, 1fr))` grid. It participates in the
    /// `Fr` sweep; a track that gets no leftover share still holds at `min`.
    FlexMin(f32, f32),
}

/// The trailing-empty-track rule for a responsive `repeat(auto-*, …)` column
/// template: `Fill` keeps every computed track (empty ones hold their place and
/// their gap); `Fit` collapses trailing tracks that hold no single-track item to
/// zero width and folds away the gap they would have introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoRepeat {
    /// `auto-fill`: keep the computed empty trailing tracks.
    Fill,
    /// `auto-fit`: collapse the computed empty trailing tracks.
    Fit,
}

/// The upper bound of a responsive `minmax(min, max)` track: a pixel cap
/// (fixed-size, resolved like [`TrackSizing::Minmax`]) or an `Fr` share
/// (flexible, resolved like [`TrackSizing::FlexMin`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackMax {
    /// A pixel upper bound: the track is content-clamped into `[min, px]`.
    Px(f32),
    /// A free-space share: the track takes `min` then an `fr` share of leftover.
    Fr(f32),
}

/// A responsive column template `repeat(auto-fill | auto-fit, minmax(min, max))`:
/// the *number* of columns is not fixed at build time but computed from the
/// container's inner width during layout, and every column is a `minmax(min, max)`
/// track. Kept as a `Copy` scalar so it rides on the grid's `LayoutInput` with no
/// heap; the common (non-adaptive) grid carries `None`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptiveColumns {
    /// Trailing-empty-track rule (`auto-fill` vs `auto-fit`).
    pub mode: AutoRepeat,
    /// Per-column pixel floor (`min` of `minmax(min, max)`); also the divisor of
    /// the column-count formula.
    pub min: f32,
    /// Per-column upper bound (`max` of `minmax(min, max)`).
    pub max: TrackMax,
}

impl AdaptiveColumns {
    /// `repeat(auto-fill, minmax(min, max))`: fit as many `minmax(min, max)`
    /// columns as the container's inner width allows, keeping computed empty
    /// trailing columns (they hold their place and their gap).
    pub fn auto_fill(min: f32, max: TrackMax) -> Self {
        AdaptiveColumns {
            mode: AutoRepeat::Fill,
            min,
            max,
        }
    }

    /// `repeat(auto-fit, minmax(min, max))`: like [`AdaptiveColumns::auto_fill`]
    /// but trailing columns that hold no single-track item collapse to zero width,
    /// folding away the gap they would have introduced (CSS collapsed tracks).
    pub fn auto_fit(min: f32, max: TrackMax) -> Self {
        AdaptiveColumns {
            mode: AutoRepeat::Fit,
            min,
            max,
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

/// Where a box sits in the room it is given, on both axes: at the start, the
/// middle or the end of each. `start` and `end` are the inline edges a line
/// begins and ends at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Alignment2D {
    /// The top start corner.
    #[default]
    TopStart,
    /// The top edge, centered across.
    Top,
    /// The top end corner.
    TopEnd,
    /// The start edge, centered down.
    Start,
    /// The middle.
    Center,
    /// The end edge, centered down.
    End,
    /// The bottom start corner.
    BottomStart,
    /// The bottom edge, centered across.
    Bottom,
    /// The bottom end corner.
    BottomEnd,
}

impl Alignment2D {
    /// Every alignment, by its wire tag.
    pub const ALL: [Self; 9] = [
        Self::TopStart,
        Self::Top,
        Self::TopEnd,
        Self::Start,
        Self::Center,
        Self::End,
        Self::BottomStart,
        Self::Bottom,
        Self::BottomEnd,
    ];

    /// How far into the slack of its room the box sits across and down: `0`
    /// at the start or top, `0.5` in the middle, `1` at the end or bottom.
    #[inline]
    pub const fn fractions(self) -> (f32, f32) {
        const FRACTION: [f32; 3] = [0.0, 0.5, 1.0];
        let tag = self as usize;
        (FRACTION[tag % 3], FRACTION[tag / 3])
    }
}

/// How a `Stack` child sits in the stack: its own alignment, `None` to take
/// the stack's, and the layer it paints in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StackPlacement {
    /// Where it sits in the stack's content box, over the stack's own.
    pub align: Option<Alignment2D>,
    /// Children paint by ascending layer, those of one layer in source order.
    pub layer: i32,
}

/// Where an `Absolute` child sits: its distance from each edge of the
/// container's content box it is pinned to, `None` for an edge it is not. A
/// percentage reads the content box's height for `top` and `bottom`, its width
/// for `start` and `end`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AbsoluteInsets {
    /// From the top edge.
    pub top: Option<Span>,
    /// From the end edge.
    pub end: Option<Span>,
    /// From the bottom edge.
    pub bottom: Option<Span>,
    /// From the start edge.
    pub start: Option<Span>,
}

impl AbsoluteInsets {
    /// Pinned to no edge.
    pub const NONE: Self = Self {
        top: None,
        end: None,
        bottom: None,
        start: None,
    };

    /// The near and far edges along `axis`: start and end on a row, top and
    /// bottom on a column.
    #[inline]
    pub fn on(&self, axis: Axis) -> (Option<Span>, Option<Span>) {
        match axis {
            Axis::Row => (self.start, self.end),
            Axis::Column => (self.top, self.bottom),
        }
    }
}

/// Where a child sits in its parent beyond its own size: a grid cell, a stack
/// alignment and layer, or absolute insets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    /// In a `Grid`.
    Grid(GridPlacement),
    /// In a `Stack`.
    Stack(StackPlacement),
    /// In an `Absolute`.
    Absolute(AbsoluteInsets),
}

impl Placement {
    /// Whether it says nothing a child without one does not: auto-flow with
    /// span 1, the stack's alignment on layer 0, or pinned to no edge.
    pub fn is_default(&self) -> bool {
        match self {
            Placement::Grid(grid) => *grid == GridPlacement::default(),
            Placement::Stack(stack) => *stack == StackPlacement::default(),
            Placement::Absolute(insets) => *insets == AbsoluteInsets::NONE,
        }
    }
}

impl From<GridPlacement> for Placement {
    fn from(grid: GridPlacement) -> Self {
        Placement::Grid(grid)
    }
}

impl From<StackPlacement> for Placement {
    fn from(stack: StackPlacement) -> Self {
        Placement::Stack(stack)
    }
}

impl From<AbsoluteInsets> for Placement {
    fn from(insets: AbsoluteInsets) -> Self {
        Placement::Absolute(insets)
    }
}
