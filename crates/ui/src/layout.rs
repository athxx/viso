//! Flex layout: sizing vocabulary plus the two-pass measure/layout algorithm,
//! a direct algorithm rather than a generic constraint solver.
//!
//! A bottom-up measure pass computes each node's natural size, then a top-down
//! layout pass hands each container its box and places children along one axis.
//! Both passes walk the retained tree over the [`crate::node::NodeArena`] and
//! read/write the parallel hot/warm side-storage arrays in
//! [`crate::component::NodeStore`], so the hot path touches compact ids and flat
//! data with no heap allocation per node.

use crate::grid::{GridPlacement, TrackSizing};
use viso_render::Rect;

/// A two-component vector in physical pixels — a scroll offset or a
/// translation. Kept in the ui tier because `viso_render` carries only `Rect`
/// and `Point`; a scroll offset is a ui-model quantity, not a render primitive.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    /// The zero vector (no offset).
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };

    /// A vector from its two components.
    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        Vec2 { x, y }
    }

    /// This vector's component along `axis` (x for Row, y for Column).
    #[inline]
    pub fn on(self, axis: Axis) -> f32 {
        match axis {
            Axis::Row => self.x,
            Axis::Column => self.y,
        }
    }
}

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
    /// A scroll viewport: a container whose box is its own requested [`Size`]
    /// but whose single content child is laid out at the content's natural main
    /// extent (not clamped to the viewport), so content exceeding the viewport
    /// along `axis` becomes scrollable overflow.
    Scroll {
        /// The scrollable axis.
        axis: Axis,
        /// The viewport's own size request within its parent.
        size: Size,
    },
    /// A canvas of absolutely-positioned rows along `axis`: each child is placed
    /// at its own row offset (read via [`LayoutTree::row_offset`]) rather than
    /// flowed. Its box is its own fixed `size` — the full logical extent of a
    /// virtualized collection — so it need not enumerate or sum the sparse set of
    /// mounted children to know its size. The scroll viewport above it reads that
    /// fixed extent as the scroll range; only the mounted rows are laid out.
    AbsoluteRows {
        /// The axis along which rows are stacked (row offsets are on this axis).
        axis: Axis,
        /// The canvas's own size request — fixed on both axes.
        size: Size,
    },
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
}

impl LayoutInput {
    /// The node's own size request within its parent, regardless of kind.
    #[inline]
    pub fn size(self) -> Size {
        match self {
            LayoutInput::Flex { size, .. }
            | LayoutInput::Leaf { size }
            | LayoutInput::Scroll { size, .. }
            | LayoutInput::AbsoluteRows { size, .. }
            | LayoutInput::Grid { size, .. } => size,
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
    /// Record a scroll viewport's content extent (the laid-out size of its
    /// content along each axis) so the scroll clamp reads it without re-walking.
    /// A no-op for non-scroll nodes; the [`LayoutInput::Scroll`] layout arm is
    /// the only caller.
    fn set_content(&mut self, index: u32, content: Vec2);
    /// The main-axis offset at which a positioned row sits inside an
    /// [`LayoutInput::AbsoluteRows`] canvas, or `None` when the node is not a
    /// positioned row. The `AbsoluteRows` layout arm is the only caller.
    fn row_offset(&self, index: u32) -> Option<f32>;
    /// The resolved column track template for a grid node, or `None` when the
    /// node is not a grid. The grid layout arm is the only caller.
    fn grid_column_tracks(&self, index: u32) -> Option<&[TrackSizing]>;
    /// The resolved row track template for a grid node, or `None` when not a grid.
    fn grid_row_tracks(&self, index: u32) -> Option<&[TrackSizing]>;
    /// A child's placement inside its grid parent (default = auto-flow, span 1).
    fn grid_placement(&self, index: u32) -> GridPlacement;
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
        LayoutInput::Scroll { size, .. } => {
            // A viewport's natural size is its own request: a Fixed axis is its
            // pixel value; a Fit/Fill axis hugs the single content child's
            // natural extent (the content can still exceed the resolved box —
            // that overflow is what scrolls, decided at layout time).
            let child_natural = |axis: Axis| -> f32 {
                if child_count > 0 {
                    tree.measured(scratch[start]).on(axis)
                } else {
                    0.0
                }
            };
            let w = match size.width {
                Length::Fixed(v) => v,
                _ => child_natural(Axis::Row),
            };
            let h = match size.height {
                Length::Fixed(v) => v,
                _ => child_natural(Axis::Column),
            };
            Measured { w, h }
        }
        LayoutInput::AbsoluteRows { size, .. } => {
            // The canvas measures to its own declared size — the full logical
            // extent — never the sum of its sparse mounted children. Both axes
            // are Fixed in normal use; a non-Fixed axis falls back to 0 (there is
            // no content sum to hug, by design).
            Measured {
                w: natural_length(size.width, 0.0),
                h: natural_length(size.height, 0.0),
            }
        }
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
            let main_cols =
                sum_axis(tree.grid_column_tracks(root), column_gap) + padding.main(Axis::Row);
            let main_rows =
                sum_axis(tree.grid_row_tracks(root), row_gap) + padding.main(Axis::Column);
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

    let (axis, gap, padding, align) = match tree.input(root) {
        LayoutInput::Flex {
            axis,
            gap,
            padding,
            align,
            ..
        } => (axis, gap, padding, align),
        LayoutInput::Scroll { axis, .. } => {
            layout_scroll(tree, root, bounds, axis, scratch);
            return;
        }
        LayoutInput::AbsoluteRows { axis, .. } => {
            layout_absolute_rows(tree, root, bounds, axis, scratch);
            return;
        }
        LayoutInput::Leaf { .. } => return, // Leaf: bounds are final.
        LayoutInput::Grid { .. } => {
            layout_grid(tree, root, bounds, scratch);
            return;
        }
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

/// Lay out a scroll viewport's single content child. The viewport already has
/// its own box (`bounds`); its content is placed at the viewport origin with its
/// natural main extent — deliberately *not* clamped to the viewport, so content
/// longer than the viewport along `axis` overflows and becomes scrollable. The
/// cross extent fills the viewport (content is only scrollable on `axis` this
/// slice). The scroll offset is not applied here: `bounds` stays the unscrolled
/// layout truth and the world transform pass shifts the subtree by `-scroll`.
///
/// The content extent (what the scroll clamp needs) is recorded via
/// [`LayoutTree::set_content`]; with no content child the extent is zero.
fn layout_scroll(
    tree: &mut impl LayoutTree,
    root: u32,
    bounds: Rect,
    axis: Axis,
    scratch: &mut Vec<u32>,
) {
    let start = scratch.len();
    tree.children(root, scratch);
    let child_count = scratch.len() - start;
    if child_count == 0 {
        scratch.truncate(start);
        tree.set_content(root, Vec2::ZERO);
        return;
    }

    let cross = cross_of(axis);
    // Content takes its natural main extent (unclamped → overflow scrolls) and
    // fills the viewport across. A viewport hosts a single content subtree; if
    // more than one child was declared, only the first is the scrolled content.
    let content = scratch[start];
    let main_size = tree.measured(content).on(axis);
    let cross_size = rect_len(bounds, cross);
    scratch.truncate(start);

    let content_box = axis_rect(
        axis,
        rect_start(bounds, axis),
        rect_start(bounds, cross),
        main_size,
        cross_size,
    );
    tree.set_content(root, axis_pack_vec(axis, main_size, cross_size));
    layout(tree, content, content_box, scratch);
}

/// Lay out an [`LayoutInput::AbsoluteRows`] canvas: place each mounted child at
/// its own row offset along `axis` rather than flowing them. The canvas already
/// has its (fixed, full-extent) box; each positioned child is placed at
/// `main = canvas_start + row_offset(child)`, takes its measured natural main
/// extent, and fills the canvas across. A child with no row offset (not a
/// positioned row) is skipped. Only the mounted children are touched, so the
/// pass cost scales with the mounted window, not the logical item count.
fn layout_absolute_rows(
    tree: &mut impl LayoutTree,
    root: u32,
    bounds: Rect,
    axis: Axis,
    scratch: &mut Vec<u32>,
) {
    let start = scratch.len();
    tree.children(root, scratch);
    let child_count = scratch.len() - start;
    if child_count == 0 {
        scratch.truncate(start);
        return;
    }

    let cross = cross_of(axis);
    let main_origin = rect_start(bounds, axis);
    let cross_origin = rect_start(bounds, cross);
    let cross_size = rect_len(bounds, cross);

    // The child ids sit in `scratch[start..start + child_count]`. A recursive
    // `layout` leaves `scratch` at the length it entered with (every arm pushes
    // its own children and truncates them back), so that window stays intact
    // across the loop — we index it directly and never snapshot, keeping the
    // pass allocation-free.
    for k in 0..child_count {
        let child = scratch[start + k];
        let Some(offset) = tree.row_offset(child) else {
            continue;
        };
        let main_size = tree.measured(child).on(axis);
        let child_box = axis_rect(
            axis,
            main_origin + offset,
            cross_origin,
            main_size,
            cross_size,
        );
        layout(tree, child, child_box, scratch);
    }
    scratch.truncate(start);
}

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

/// A [`Vec2`] from a main/cross pair for a given axis.
#[inline]
fn axis_pack_vec(axis: Axis, main: f32, cross: f32) -> Vec2 {
    match axis {
        Axis::Row => Vec2 { x: main, y: cross },
        Axis::Column => Vec2 { x: cross, y: main },
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

#[cfg(test)]
mod tests {
    use super::*;

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
        // Four fill children auto-flow into the four cells. Each child is a
        // track-less grid node with a fill size, which behaves as a fill leaf
        // (it fills its cell and, having no children, adds no further layout).
        let mut kids = Vec::new();
        for _ in 0..4 {
            let k = store.alloc_grid(GridStyle {
                columns: Vec::new(),
                rows: Vec::new(),
                size: Size::fill(),
                ..Default::default()
            });
            store.arena_append_child(grid, k);
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
}
