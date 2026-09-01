//! Flex layout: sizing vocabulary plus the two-pass measure/layout algorithm
//! (§12.2 direct algorithm, not a generic constraint solver).
//!
//! A bottom-up measure pass (phase 5) computes each node's natural size, then a
//! top-down layout pass (phase 6) hands each container its box and places
//! children along one axis. Both passes walk the retained tree over the
//! [`crate::node::NodeArena`] and read/write the parallel side-storage arrays in
//! [`crate::component::NodeStore`] (§8.4 hot/warm/cold), so the hot path touches
//! compact ids and flat data with no heap allocation per node (§7.1).

use viso_render::Rect;

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
/// A `Fixed` length is a hard pixel size. A `Fill` length claims a share of the
/// leftover space along the main axis, split between siblings by `weight`. A
/// `Fit` length shrinks to the node's measured natural size. The measure pass
/// already computes a natural size for every node, so `Fit` is a first-class
/// citizen even though this slice's containers drive `Fixed` and `Fill`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    /// A hard pixel length.
    Fixed(f32),
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
}

/// A node's requested size on both axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    /// Width request.
    pub width: Length,
    /// Height request.
    pub height: Length,
}

impl Size {
    /// A hard-pixel box.
    pub const fn fixed(w: f32, h: f32) -> Self {
        Size {
            width: Length::Fixed(w),
            height: Length::Fixed(h),
        }
    }

    /// A unit-weight main-axis fill on both axes.
    pub const fn fill() -> Self {
        Size {
            width: Length::Fill { weight: 1.0 },
            height: Length::Fill { weight: 1.0 },
        }
    }

    /// The requested length on a given axis.
    #[inline]
    pub fn on(self, axis: Axis) -> Length {
        match axis {
            Axis::Row => self.width,
            Axis::Column => self.height,
        }
    }

    /// The requested length on the axis crossing `axis`.
    #[inline]
    pub fn cross(self, axis: Axis) -> Length {
        match axis {
            Axis::Row => self.height,
            Axis::Column => self.width,
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
    fn main(self, axis: Axis) -> f32 {
        match axis {
            Axis::Row => self.left + self.right,
            Axis::Column => self.top + self.bottom,
        }
    }

    /// Total inset across the cross axis (both edges).
    #[inline]
    fn cross(self, axis: Axis) -> f32 {
        match axis {
            Axis::Row => self.top + self.bottom,
            Axis::Column => self.left + self.right,
        }
    }

    /// Near-edge inset along the main axis (left for Row, top for Column).
    #[inline]
    fn main_start(self, axis: Axis) -> f32 {
        match axis {
            Axis::Row => self.left,
            Axis::Column => self.top,
        }
    }

    /// Near-edge inset across the cross axis (top for Row, left for Column).
    #[inline]
    fn cross_start(self, axis: Axis) -> f32 {
        match axis {
            Axis::Row => self.top,
            Axis::Column => self.left,
        }
    }
}

/// The warm-tier layout parameters for one node: either a Flex container or a
/// leaf with a requested [`Size`]. A leaf carries its own size; a container
/// carries its axis, child gap, padding, and cross-axis alignment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LayoutInput {
    /// A Flex container that arranges its children along `axis`.
    Flex {
        /// Main axis.
        axis: Axis,
        /// Gap inserted between adjacent children along the main axis.
        gap: f32,
        /// Inner padding on all four edges.
        padding: Inset,
        /// Cross-axis alignment of children.
        align: Align,
        /// The container's own size request within its parent.
        size: Size,
    },
    /// A leaf that occupies its requested [`Size`].
    Leaf {
        /// Size request within the parent.
        size: Size,
    },
}

impl LayoutInput {
    /// The node's own size request within its parent, regardless of kind.
    #[inline]
    pub fn size(self) -> Size {
        match self {
            LayoutInput::Flex { size, .. } | LayoutInput::Leaf { size } => size,
        }
    }
}

/// The natural (content) size a node measures to, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Measured {
    /// Natural width.
    pub w: f32,
    /// Natural height.
    pub h: f32,
}

impl Measured {
    /// The natural extent along a given axis.
    #[inline]
    fn on(self, axis: Axis) -> f32 {
        match axis {
            Axis::Row => self.w,
            Axis::Column => self.h,
        }
    }
}

/// Resolve a single [`Length`] to its natural (measure-time) contribution: a
/// `Fixed` is its value, a `Fit` is the already-measured natural extent, and a
/// `Fill` contributes only its measured base (0 for a bare fill leaf) since its
/// real size is decided by the parent's leftover-space distribution.
#[inline]
fn natural_length(length: Length, measured_natural: f32) -> f32 {
    match length {
        Length::Fixed(v) => v,
        Length::Fit => measured_natural,
        Length::Fill { .. } => measured_natural,
    }
}

/// Read-only view a measure/layout pass needs over one node: its layout input,
/// its children in order, and mutable access to its measured/bounds slots. The
/// passes below are written against these free functions so
/// [`crate::component::NodeStore`] can supply the storage without this module
/// depending on its concrete field layout.
///
/// The passes recurse over `children`, which returns child ids in sibling
/// order. Storage is indexed by [`crate::node::NodeId::index`].
pub trait LayoutTree {
    /// The layout input for a node index.
    fn input(&self, index: u32) -> LayoutInput;
    /// The child node indices of a node, in order, appended to `out`.
    fn children(&self, index: u32, out: &mut Vec<u32>);
    /// Read a node's measured natural size.
    fn measured(&self, index: u32) -> Measured;
    /// Write a node's measured natural size.
    fn set_measured(&mut self, index: u32, m: Measured);
    /// Write a node's resolved layout box.
    fn set_bounds(&mut self, index: u32, r: Rect);
}

/// Bottom-up measure pass: compute every node's natural size.
///
/// A leaf's natural size resolves each axis's [`Length`] against its measured
/// content (0 for a bare fill/fit leaf in this slice). A Flex container's main
/// natural size is the sum of children's main naturals plus `gap` between each
/// adjacent pair plus main padding; its cross natural is the max child cross
/// plus cross padding. The recursion is post-order so children are measured
/// before their parent reads them. `scratch` is a reusable child-id buffer so
/// the walk allocates nothing per node.
pub fn measure(tree: &mut impl LayoutTree, root: u32, scratch: &mut Vec<u32>) {
    let start = scratch.len();
    tree.children(root, scratch);
    let child_count = scratch.len() - start;

    // Recurse first (post-order): measure children before folding them in.
    // Drain this node's slice as we go so nested measures reuse the tail.
    for i in 0..child_count {
        let child = scratch[start + i];
        measure(tree, child, scratch);
    }

    let measured = match tree.input(root) {
        LayoutInput::Leaf { size } => Measured {
            w: natural_length(size.width, 0.0),
            h: natural_length(size.height, 0.0),
        },
        LayoutInput::Flex {
            axis,
            gap,
            padding,
            size,
            ..
        } => {
            let mut main_sum = 0.0f32;
            let mut cross_max = 0.0f32;
            for i in 0..child_count {
                let child = scratch[start + i];
                let cm = tree.measured(child);
                main_sum += cm.on(axis);
                cross_max = cross_max.max(cm.on(cross_of(axis)));
            }
            if child_count > 1 {
                main_sum += gap * (child_count as f32 - 1.0);
            }
            main_sum += padding.main(axis);
            cross_max += padding.cross(axis);

            // A container may itself be Fixed on an axis; honor that over the
            // content sum so a fixed-size box measures to its declared size.
            let main_natural = match size.on(axis) {
                Length::Fixed(v) => v,
                _ => main_sum,
            };
            let cross_natural = match size.cross(axis) {
                Length::Fixed(v) => v,
                _ => cross_max,
            };
            axis_pack(axis, main_natural, cross_natural)
        }
    };

    tree.set_measured(root, measured);
    scratch.truncate(start);
}

/// Top-down layout pass: place `root` into `bounds`, then place its children.
///
/// A Flex container distributes its inner main extent (its box minus main
/// padding and inter-child gaps) among children: fixed/fit children take their
/// natural main size; fill children split the leftover by weight (0 when there
/// is no leftover). Cross size and offset come from `align`. Children are then
/// laid recursively into the boxes computed here. `scratch` is a reusable
/// child-id buffer.
pub fn layout(tree: &mut impl LayoutTree, root: u32, bounds: Rect, scratch: &mut Vec<u32>) {
    tree.set_bounds(root, bounds);

    let LayoutInput::Flex {
        axis,
        gap,
        padding,
        align,
        ..
    } = tree.input(root)
    else {
        return; // Leaf: bounds are final.
    };

    let start = scratch.len();
    tree.children(root, scratch);
    let child_count = scratch.len() - start;
    if child_count == 0 {
        scratch.truncate(start);
        return;
    }

    let cross = cross_of(axis);
    let main_extent = rect_len(bounds, axis) - padding.main(axis);
    let cross_extent = rect_len(bounds, cross) - padding.cross(axis);
    let gaps_total = if child_count > 1 {
        gap * (child_count as f32 - 1.0)
    } else {
        0.0
    };

    // Sum the fixed/fit main sizes and the total fill weight in one sweep.
    let mut fixed_main = 0.0f32;
    let mut weight_total = 0.0f32;
    for i in 0..child_count {
        let child = scratch[start + i];
        match tree.input(child).size().on(axis) {
            Length::Fixed(v) => fixed_main += v,
            Length::Fit => fixed_main += tree.measured(child).on(axis),
            Length::Fill { weight } => weight_total += weight.max(0.0),
        }
    }

    let free = (main_extent - fixed_main - gaps_total).max(0.0);

    // Place children along the main axis, advancing a cursor from the near edge.
    let main_origin = rect_start(bounds, axis) + padding.main_start(axis);
    let cross_origin = rect_start(bounds, cross) + padding.cross_start(axis);
    let mut cursor = main_origin;

    // Snapshot child ids before recursing (recursion reuses `scratch`).
    let children: Vec<u32> = scratch[start..start + child_count].to_vec();
    scratch.truncate(start);

    for &child in &children {
        let size = tree.input(child).size();
        let main_size = match size.on(axis) {
            Length::Fixed(v) => v,
            Length::Fit => tree.measured(child).on(axis),
            Length::Fill { weight } => {
                if weight_total > 0.0 {
                    free * (weight.max(0.0) / weight_total)
                } else {
                    0.0
                }
            }
        };

        let natural_cross = tree.measured(child).on(cross);
        let (cross_size, cross_off) = match align {
            Align::Stretch => (cross_extent, 0.0),
            Align::Start => (natural_cross, 0.0),
            Align::Center => (natural_cross, (cross_extent - natural_cross) * 0.5),
            Align::End => (natural_cross, cross_extent - natural_cross),
        };
        // A cross-axis Fixed request overrides alignment-derived sizing.
        let cross_size = match (size.cross(axis), align) {
            (Length::Fixed(v), _) => v,
            _ => cross_size,
        };

        let child_box = axis_rect(
            axis,
            cursor,
            cross_origin + cross_off,
            main_size,
            cross_size,
        );
        layout(tree, child, child_box, scratch);

        cursor += main_size + gap;
    }
}

/// The axis crossing `axis`.
#[inline]
fn cross_of(axis: Axis) -> Axis {
    match axis {
        Axis::Row => Axis::Column,
        Axis::Column => Axis::Row,
    }
}

/// A [`Measured`] from a main/cross pair for a given axis.
#[inline]
fn axis_pack(axis: Axis, main: f32, cross: f32) -> Measured {
    match axis {
        Axis::Row => Measured { w: main, h: cross },
        Axis::Column => Measured { w: cross, h: main },
    }
}

/// A rect's extent along a given axis.
#[inline]
fn rect_len(r: Rect, axis: Axis) -> f32 {
    match axis {
        Axis::Row => r.w,
        Axis::Column => r.h,
    }
}

/// A rect's near-edge coordinate along a given axis.
#[inline]
fn rect_start(r: Rect, axis: Axis) -> f32 {
    match axis {
        Axis::Row => r.x,
        Axis::Column => r.y,
    }
}

/// Build a rect from main/cross origins and extents for a given main axis.
#[inline]
fn axis_rect(axis: Axis, main_pos: f32, cross_pos: f32, main_len: f32, cross_len: f32) -> Rect {
    match axis {
        Axis::Row => Rect {
            x: main_pos,
            y: cross_pos,
            w: main_len,
            h: cross_len,
        },
        Axis::Column => Rect {
            x: cross_pos,
            y: main_pos,
            w: cross_len,
            h: main_len,
        },
    }
}
