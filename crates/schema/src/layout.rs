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
