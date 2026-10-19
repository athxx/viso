//! The layout vocabulary: axes, lengths, alignment and insets.

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
/// `Fit` length shrinks to the node's measured natural size.
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
    /// Shrink to the measured natural size.
    Fit,
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

    /// The extent this length takes against `basis`, or `None` for `Fill`/`Fit`,
    /// whose extent the container decides.
    #[inline]
    pub fn resolve(self, basis: f32) -> Option<f32> {
        match self {
            Length::Fixed(v) => Some(v),
            Length::Relative { fixed, pct } => Some((fixed + pct * basis).max(0.0)),
            Length::Fill { .. } | Length::Fit => None,
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

/// How a grid cell's content is aligned within the cell along the block (row /
/// vertical) axis. `Stretch` (the default) grows a fillable child to the cell
/// height; the others place the child at its natural height and shift it.
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
