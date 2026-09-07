//! Flex layout: sizing vocabulary plus the two-pass measure/layout algorithm,
//! a direct algorithm rather than a generic constraint solver.
//!
//! A bottom-up measure pass computes each node's natural size, then a top-down
//! layout pass hands each container its box and places children along one axis.
//! Both passes walk the retained tree over the [`crate::node::NodeArena`] and
//! read/write the parallel hot/warm side-storage arrays in
//! [`crate::component::NodeStore`], so the hot path touches compact ids and flat
//! data with no heap allocation per node.

use crate::grid::{AdaptiveColumns, AutoRepeat, GridPlacement, TrackMax, TrackSizing};
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

    /// Linearly interpolate toward `to` by `t`: `t == 0` returns `self`,
    /// `t == 1` returns `to`. Used to advance a translate animation from its
    /// start offset to its target as the eased progress sweeps `[0, 1]`.
    #[inline]
    pub fn lerp(self, to: Vec2, t: f32) -> Vec2 {
        Vec2 {
            x: self.x + (to.x - self.x) * t,
            y: self.y + (to.y - self.y) * t,
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
        /// Block-axis (vertical) alignment of each cell's content within its cell.
        align_items: AlignItems,
        /// This grid is a subgrid on the column (inline) axis: it adopts its
        /// parent grid's resolved column tracks over its cell span instead of
        /// solving its own column template. Only meaningful when this grid is a
        /// child of another grid; ignored otherwise (falls back to self-solve).
        subgrid_columns: bool,
        /// This grid is a subgrid on the row (block) axis (see `subgrid_columns`).
        subgrid_rows: bool,
        /// Responsive column template `repeat(auto-fill | auto-fit, minmax(min,
        /// max))`: when `Some`, the column *count* is solved from the container's
        /// inner width at layout time and every column is a `minmax(min, max)`
        /// track, replacing the explicit `column_count`/column template on the
        /// inline axis. A `Copy` scalar, `None` on the common grid. Ignored when
        /// the column axis is inherited by a subgrid.
        adaptive_columns: Option<AdaptiveColumns>,
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
    /// A node's content intrinsic size, or `None` when it carries no drawable
    /// content. A `Fit`/`Fill` leaf axis resolves against this; a `None` leaf
    /// measures to `0` on both axes (a bare layout leaf, the prior behavior).
    fn content_natural(&self, index: u32) -> Option<Vec2>;
    /// A node's first-line text baseline in physical pixels, or `None` when it
    /// carries no text content. Read only by a grid cell aligned on the baseline
    /// (`AlignItems::Baseline`); the common node never touches it.
    fn content_baseline(&self, index: u32) -> Option<f32>;
    /// Whether a grid node is a subgrid on each axis: `(columns, rows)`. A
    /// subgrid axis adopts its parent grid's resolved tracks over the child's
    /// cell span instead of solving its own template. `(false, false)` for a
    /// non-subgrid grid or a non-grid node; read only by the grid layout arm
    /// when it lays out a grid child of a grid.
    fn subgrid_axes(&self, index: u32) -> (bool, bool);
    /// Whether a node (and its subtree) is folded out of layout. A hidden node
    /// measures to zero on both axes and lays out to a zero rect, contributing
    /// nothing to a parent's main-axis sum, without a structural rebuild.
    fn hidden(&self, index: u32) -> bool;
}

/// Bottom-up measure pass: compute every node's natural size.
///
/// A leaf's natural size resolves each axis's [`Length`] against its content
/// intrinsic size (0 for a bare layout leaf with no content). A Flex container's main
/// natural size is the sum of children's main naturals plus `gap` between each
/// adjacent pair plus main padding; its cross natural is the max child cross
/// plus cross padding. The recursion is post-order so children are measured
/// before their parent reads them. `scratch` is a reusable child-id buffer so
/// the walk allocates nothing per node.
pub fn measure(tree: &mut impl LayoutTree, root: u32, scratch: &mut Vec<u32>) {
    // A hidden subtree folds out of layout: it measures to zero (contributing
    // nothing to a parent's main-axis sum) and its children are never walked, so
    // it reserves no scratch and does not recurse.
    if tree.hidden(root) {
        tree.set_measured(root, Measured { w: 0.0, h: 0.0 });
        return;
    }
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
        LayoutInput::Leaf { size } => {
            // A content leaf (text/image/path) resolves a Fit/Fill axis against
            // its measured intrinsic size; a bare layout leaf has none and reads
            // back 0 on both axes.
            let content = tree.content_natural(root).unwrap_or(Vec2::ZERO);
            Measured {
                w: natural_length(size.width, content.x),
                h: natural_length(size.height, content.y),
            }
        }
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
    // A hidden subtree lays out to a zero rect at the parent-assigned origin and
    // its children are never placed — it measured to zero, so the parent already
    // gave it a zero-extent slot; short-circuiting here also spares the whole
    // subtree's layout recursion.
    if tree.hidden(root) {
        tree.set_bounds(
            root,
            Rect {
                x: bounds.x,
                y: bounds.y,
                w: 0.0,
                h: 0.0,
            },
        );
        return;
    }
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
            // Top-level grid (or a grid child of a non-grid parent): it solves
            // both of its own axes — no inherited tracks.
            layout_grid(tree, root, bounds, None, None, scratch);
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

    // The child ids live in `scratch[start..start + child_count]`. Recursion
    // appends past that range and truncates back to its own start, so the ids
    // stay valid across the loop — no per-container snapshot allocation. Each
    // id is `Copy`d out before the recursive `&mut scratch` borrow.
    for i in 0..child_count {
        let child = scratch[start + i];
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

    // Release the child-id slice back to the caller's scratch high-water mark.
    scratch.truncate(start);
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
/// The reusable per-`layout_grid`-call buffers. `layout_grid` needs about a
/// dozen temporary `Vec`s (placements, the occupied bitset, cell regions, the
/// two track templates, per-track auto maxes, resolved sizes, prefix offsets,
/// and row baselines). Allocating them fresh on every call would heap-allocate
/// on the layout hot path for every grid node every frame; instead each call
/// checks a `GridScratch` out of a thread-local pool ([`with_grid_scratch`]),
/// clears the buffers (retaining capacity — clear, not free), and returns it on
/// exit, so a warmed-up steady-state grid relayout does no per-frame allocation.
///
/// A pool rather than a single threaded buffer because `layout_grid` is
/// *reentrant*: a subgrid child recurses into `layout_grid` from inside the
/// parent's per-child loop while the parent still holds live slices of its own
/// `col_sizes`/`row_sizes`. The recursive call checks out a *distinct*
/// `GridScratch`, so nested grids never clobber an ancestor's buffers, and the
/// pool grows only to the deepest grid nesting seen (one entry for the common
/// non-nested case).
#[derive(Default)]
struct GridScratch {
    placements: Vec<GridPlacement>,
    occupied: Vec<u64>,
    regions: Vec<crate::grid::CellRegion>,
    col_tracks: Vec<TrackSizing>,
    row_tracks: Vec<TrackSizing>,
    col_auto: Vec<f32>,
    row_auto: Vec<f32>,
    col_sizes: Vec<f32>,
    row_sizes: Vec<f32>,
    col_offsets: Vec<f32>,
    row_offsets: Vec<f32>,
    row_baselines: Vec<f32>,
}

impl GridScratch {
    /// Drop every buffer's contents while keeping its capacity, so the next
    /// checkout reuses the same allocations (clear, not free).
    fn clear(&mut self) {
        self.placements.clear();
        self.occupied.clear();
        self.regions.clear();
        self.col_tracks.clear();
        self.row_tracks.clear();
        self.col_auto.clear();
        self.row_auto.clear();
        self.col_sizes.clear();
        self.row_sizes.clear();
        self.col_offsets.clear();
        self.row_offsets.clear();
        self.row_baselines.clear();
    }
}

thread_local! {
    /// Free-list of grid scratch buffers, one per live grid nesting level. The
    /// layout pass is main-thread-owned (section 26), so a thread-local pool
    /// carries no locking and no cross-frame allocation once warm.
    static GRID_SCRATCH_POOL: std::cell::RefCell<Vec<GridScratch>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Run `f` with a cleared `GridScratch` checked out of the thread-local pool,
/// returning it to the pool afterward. Reentrant: a nested call checks out a
/// distinct buffer, so a subgrid recursion never aliases its ancestor's scratch.
fn with_grid_scratch<R>(f: impl FnOnce(&mut GridScratch) -> R) -> R {
    let mut s = GRID_SCRATCH_POOL
        .with(|p| p.borrow_mut().pop())
        .unwrap_or_default();
    s.clear();
    let r = f(&mut s);
    GRID_SCRATCH_POOL.with(|p| p.borrow_mut().push(s));
    r
}

/// column and row track extents (Fixed/Percent/Auto/Fr), then lay each child
/// into its cell rect honoring its own `Size`. The grid's inner content area is
/// its box minus padding; cell offsets are prefix sums of the resolved tracks
/// with the gap between adjacent tracks. Auto tracks use the max natural main
/// size of the single-track children on them.
///
/// `inherited_cols` / `inherited_rows`, when `Some`, are the parent grid's
/// resolved track sizes sliced to this grid's cell span on that axis, paired
/// with the parent's gap on that axis. A subgrid axis uses them verbatim —
/// skipping its own template build, auto-max collection, and `solve_tracks`,
/// and adopting the parent's gap for its prefix-offset math — so its cell lines
/// coincide exactly with the parent's grid lines (interior gaps included).
/// `None` means the axis self-solves (the top-level and non-subgrid case). The
/// public `layout()` dispatcher always passes `(None, None)`; only the per-child
/// loop below passes inherited slices, and only for a child that declares itself
/// a subgrid.
#[allow(clippy::too_many_arguments)]
fn layout_grid(
    tree: &mut impl LayoutTree,
    root: u32,
    bounds: Rect,
    inherited_cols: Option<(&[f32], f32)>,
    inherited_rows: Option<(&[f32], f32)>,
    scratch: &mut Vec<u32>,
) {
    let LayoutInput::Grid {
        column_count,
        column_gap,
        row_gap,
        padding,
        auto_rows,
        align_items,
        adaptive_columns,
        ..
    } = tree.input(root)
    else {
        return;
    };
    // A subgrid inherits its parent's columns, so adaptive column-count solving is
    // ignored on an inherited column axis (the parent's resolved tracks win).
    let adaptive_columns = if inherited_cols.is_some() {
        None
    } else {
        adaptive_columns
    };
    // On a subgrid axis, adopt the parent's gap so interior cell lines land on
    // the parent's grid lines; a self-solved axis keeps its own gap.
    let column_gap = inherited_cols.map(|(_, g)| g).unwrap_or(column_gap);
    let row_gap = inherited_rows.map(|(_, g)| g).unwrap_or(row_gap);

    // Record this grid's own box. The public `layout()` dispatcher already set
    // it for the top-level entry, but the subgrid recursive entry bypasses that
    // dispatcher, so set it here to cover both paths.
    tree.set_bounds(root, bounds);

    // The child ids stay in `scratch[start..start + child_count]` for the whole
    // pass. Recursion (final loop) appends past that range and truncates back,
    // so the slice remains valid — no snapshot allocation, matching the flex
    // path. Each id is `Copy`d out before any `&mut scratch` recursion.
    let start = scratch.len();
    tree.children(root, scratch);
    let child_count = scratch.len() - start;
    if child_count == 0 {
        scratch.truncate(start);
        return;
    }

    // Inner content area (box minus padding). Hoisted above the column-count
    // solve because an adaptive template derives its column count from the inner
    // width; both depend only on `bounds`/`padding`, so this has no side effects.
    let content_w = (rect_len(bounds, Axis::Row) - padding.main(Axis::Row)).max(0.0);
    let content_h = (rect_len(bounds, Axis::Column) - padding.main(Axis::Column)).max(0.0);

    // On a column-subgrid axis the inherited slice length is the authoritative
    // column count (the node's own column template is ignored), so placement and
    // auto-flow wrap over the parent's columns, not the child's declared count.
    // An adaptive template instead fits `floor((width + gap) / (min + gap))`
    // columns into the inner width (at least one), the CSS `repeat(auto-*,
    // minmax(min, …))` count. Otherwise the explicit `column_count` stands.
    let cols = match inherited_cols {
        Some((sizes, _)) => (sizes.len() as u16).max(1),
        None => match adaptive_columns {
            Some(a) => adaptive_column_count(a.min, column_gap, content_w),
            None => column_count.max(1),
        },
    };

    // Check a `GridScratch` out of the thread-local pool for the ~dozen working
    // buffers, instead of allocating them fresh per call. `with_grid_scratch`
    // clears (not frees) and returns it on exit; a subgrid recursion below
    // checks out a distinct buffer, so it never clobbers `s`.
    with_grid_scratch(|s| {
        s.placements
            .extend((0..child_count).map(|i| tree.grid_placement(scratch[start + i])));

        // Placement pass.
        let row_used =
            crate::grid::place_children(cols, &s.placements, &mut s.occupied, &mut s.regions);

        // `auto-fit`: collapse the trailing columns that hold no item at all to
        // zero width. A column is occupied if any child's column span covers it
        // (span-1 or wider) — not merely if a span-1 item starts on it — so a
        // spanning item pins its trailing tracks open. The collapse is decided
        // here, before the solve, so the surviving tracks divide the whole
        // content width (a `1fr` max then stretches the occupied columns to fill
        // it). `auto-fill` keeps every column, so its collapsed tail is zero.
        let collapsed_tail = if matches!(adaptive_columns, Some(a) if a.mode == AutoRepeat::Fit) {
            let ncols = cols as usize;
            let mut occupied_to = 0usize; // one past the last occupied column
            for i in 0..child_count {
                let r = s.regions[i];
                let end = (r.col as usize + r.col_span as usize).min(ncols);
                occupied_to = occupied_to.max(end);
            }
            ncols - occupied_to
        } else {
            0
        };
        // The solve runs over only the surviving (non-collapsed) columns.
        let solved_cols = cols as usize - collapsed_tail;

        // Build the column track template (explicit) and the row template extended
        // with implicit `auto_rows` up to `row_used`. A subgrid axis skips its own
        // template entirely — it will adopt the parent's resolved sizes below. An
        // adaptive template emits `solved_cols` identical `minmax(min, max)`
        // tracks: a pixel `max` reuses the fixed-size `Minmax`, an `Fr` `max`
        // becomes a `FlexMin` so it takes `min` then a share of the leftover free
        // space.
        if inherited_cols.is_none() {
            match adaptive_columns {
                Some(a) => {
                    let track = match a.max {
                        TrackMax::Px(hi) => TrackSizing::Minmax(a.min, hi),
                        TrackMax::Fr(fr) => TrackSizing::FlexMin(a.min, fr),
                    };
                    s.col_tracks.extend(std::iter::repeat_n(track, solved_cols));
                }
                None => match tree.grid_column_tracks(root) {
                    Some(t) if !t.is_empty() => s.col_tracks.extend_from_slice(t),
                    _ => s.col_tracks.extend(std::iter::repeat_n(
                        crate::grid::TrackSizing::Auto,
                        cols as usize,
                    )),
                },
            }
        }
        if inherited_rows.is_none() {
            if let Some(t) = tree.grid_row_tracks(root) {
                s.row_tracks.extend_from_slice(t);
            }
            while (s.row_tracks.len() as u16) < row_used {
                s.row_tracks.push(auto_rows);
            }
        }

        // Auto maxes: for each track, the max natural main size of the span-1 items
        // whose start lies on that track. Span-1 items set the baseline; spanning
        // items then distribute their content across the growable tracks they cover
        // (see `distribute_spanning_auto`), so a wide 2-column item is not left to
        // collapse the intrinsic tracks under it. Skipped on a subgrid axis, whose
        // sizes come from the parent, not from its own content.
        s.col_auto.resize(s.col_tracks.len(), 0.0);
        s.row_auto.resize(s.row_tracks.len(), 0.0);
        for i in 0..child_count {
            let child = scratch[start + i];
            let r = s.regions[i];
            let cm = tree.measured(child);
            if r.col_span == 1 {
                let c = r.col as usize;
                if c < s.col_auto.len() {
                    s.col_auto[c] = s.col_auto[c].max(cm.on(Axis::Row));
                }
            }
            if r.row_span == 1 {
                let rr = r.row as usize;
                if rr < s.row_auto.len() {
                    s.row_auto[rr] = s.row_auto[rr].max(cm.on(Axis::Column));
                }
            }
        }
        if inherited_cols.is_none() {
            crate::grid::distribute_spanning_auto(
                &s.col_tracks,
                column_gap,
                content_w,
                (0..child_count).map(|i| {
                    let r = s.regions[i];
                    (
                        r.col,
                        r.col_span,
                        tree.measured(scratch[start + i]).on(Axis::Row),
                    )
                }),
                &mut s.col_auto,
            );
        }
        if inherited_rows.is_none() {
            crate::grid::distribute_spanning_auto(
                &s.row_tracks,
                row_gap,
                content_h,
                (0..child_count).map(|i| {
                    let r = s.regions[i];
                    (
                        r.row,
                        r.row_span,
                        tree.measured(scratch[start + i]).on(Axis::Column),
                    )
                }),
                &mut s.row_auto,
            );
        }

        // Solve each self-solved axis; a subgrid axis instead adopts the parent's
        // resolved track sizes over its cell span verbatim, so its cell boundaries
        // land exactly on the parent's grid lines.
        match inherited_cols {
            Some((sizes, _)) => s.col_sizes.extend_from_slice(sizes),
            None => crate::grid::solve_tracks(
                &s.col_tracks,
                column_gap,
                content_w,
                &s.col_auto,
                &mut s.col_sizes,
            ),
        }
        match inherited_rows {
            Some((sizes, _)) => s.row_sizes.extend_from_slice(sizes),
            None => crate::grid::solve_tracks(
                &s.row_tracks,
                row_gap,
                content_h,
                &s.row_auto,
                &mut s.row_sizes,
            ),
        }

        // The `auto-fit` solve ran over only the surviving columns; pad the
        // collapsed trailing columns back in as zero-width tracks so downstream
        // span/offset index math still addresses every original column.
        if collapsed_tail > 0 && inherited_cols.is_none() {
            s.col_sizes.resize(cols as usize, 0.0);
        }

        // Prefix-sum track offsets (with gaps) from the padded content origin.
        let origin_x = rect_start(bounds, Axis::Row) + padding.main_start(Axis::Row);
        let origin_y = rect_start(bounds, Axis::Column) + padding.main_start(Axis::Column);
        prefix_offsets_into_collapsed(&mut s.col_offsets, &s.col_sizes, column_gap, collapsed_tail);
        prefix_offsets_into(&mut s.row_offsets, &s.row_sizes, row_gap);

        // For `AlignItems::Baseline` each row shares a baseline: the max first-line
        // baseline of the cells starting on that row. A cell aligns its own baseline
        // to that shared line, so mixed-font-size cells sit on one text line. A cell
        // whose content has no text baseline contributes none and falls back to Start.
        if align_items == AlignItems::Baseline {
            s.row_baselines.resize(s.row_sizes.len(), 0.0);
            for i in 0..child_count {
                let child = scratch[start + i];
                if let Some(base) = tree.content_baseline(child) {
                    let row = s.regions[i].row as usize;
                    if base > s.row_baselines[row] {
                        s.row_baselines[row] = base;
                    }
                }
            }
        }

        // Lay each child into its cell rect. A cell spanning k tracks measures
        // offset(start) .. offset(start+span) minus the trailing gap.
        for i in 0..child_count {
            let child = scratch[start + i];
            let r = s.regions[i];
            let cx0 = s.col_offsets[r.col as usize];
            let cx1 = span_end(&s.col_offsets, &s.col_sizes, r.col, r.col_span, column_gap);
            let ry0 = s.row_offsets[r.row as usize];
            let ry1 = span_end(&s.row_offsets, &s.row_sizes, r.row, r.row_span, row_gap);
            let cell = Rect {
                x: origin_x + cx0,
                y: origin_y + ry0,
                w: (cx1 - cx0).max(0.0),
                h: (ry1 - ry0).max(0.0),
            };
            // Horizontal (inline axis): the child fills its cell when it requests
            // Fill; a Fixed/Fit child hugs its own size at the cell's left edge.
            let size = tree.input(child).size();
            let cw = match size.width {
                Length::Fixed(v) => v,
                Length::Fit => tree.measured(child).on(Axis::Row),
                Length::Fill { .. } => cell.w,
            };
            // Vertical (block axis): a `Fill` child stretches to the cell unless the
            // grid overrides with a non-Stretch `align_items`; a Fixed/Fit child
            // always hugs its own size and is then positioned by `align_items`.
            let stretch =
                matches!(size.height, Length::Fill { .. }) && align_items == AlignItems::Stretch;
            let ch = if stretch {
                cell.h
            } else {
                match size.height {
                    Length::Fixed(v) => v,
                    Length::Fit | Length::Fill { .. } => tree.measured(child).on(Axis::Column),
                }
            };
            // Block-axis offset of the child within its cell per `align_items`.
            // Baseline aligns the child's own baseline to the row's shared baseline,
            // falling back to Start when the child has no text baseline.
            let dy = match align_items {
                AlignItems::Stretch | AlignItems::Start => 0.0,
                AlignItems::Center => (cell.h - ch) * 0.5,
                AlignItems::End => cell.h - ch,
                AlignItems::Baseline => match tree.content_baseline(child) {
                    Some(base) => s.row_baselines[r.row as usize] - base,
                    None => 0.0,
                },
            };
            let child_box = Rect {
                x: cell.x,
                y: cell.y + dy,
                w: cw,
                h: ch,
            };
            // A grid child that declares itself a subgrid adopts this grid's
            // resolved tracks over its own cell span, so its inner cell lines
            // coincide with this grid's lines. We slice `col_sizes`/`row_sizes` at
            // the child's region and hand them to a dedicated `layout_grid` entry
            // (the child never routes through the generic `layout()` dispatcher, so
            // the public layout signature is untouched). That recursive call checks
            // out a distinct `GridScratch` from the pool, so these slices of `s`
            // stay valid across it. A non-subgrid child, and any non-grid child,
            // takes the ordinary path.
            let (sub_cols, sub_rows) = match tree.input(child) {
                LayoutInput::Grid { .. } => tree.subgrid_axes(child),
                _ => (false, false),
            };
            if sub_cols || sub_rows {
                let ci = |axis_start: u16, span: u16, sizes: &[f32]| {
                    let a = (axis_start as usize).min(sizes.len());
                    let b = (a + span as usize).min(sizes.len());
                    a..b
                };
                let col_slice = sub_cols.then(|| {
                    (
                        &s.col_sizes[ci(r.col, r.col_span, &s.col_sizes)],
                        column_gap,
                    )
                });
                let row_slice =
                    sub_rows.then(|| (&s.row_sizes[ci(r.row, r.row_span, &s.row_sizes)], row_gap));
                layout_grid(tree, child, child_box, col_slice, row_slice, scratch);
            } else {
                layout(tree, child, child_box, scratch);
            }
        }
    });

    // Release the child-id slice back to the caller's scratch high-water mark.
    scratch.truncate(start);
}

/// The number of columns a `repeat(auto-fill | auto-fit, minmax(min, …))`
/// template fits into `content_w`: `floor((content_w + gap) / (min + gap))`,
/// clamped to at least one. Adding one `gap` to both terms models that `n` tracks
/// carry `n − 1` interior gaps, so `n` columns of `min` fit when `n·min + (n−1)·gap
/// ≤ content_w`. A non-positive `min` degrades to a single column.
fn adaptive_column_count(min: f32, gap: f32, content_w: f32) -> u16 {
    let min = min.max(0.0);
    let gap = gap.max(0.0);
    let denom = min + gap;
    if denom <= 0.0 {
        return 1;
    }
    let n = ((content_w + gap) / denom).floor();
    if n.is_finite() && n >= 1.0 {
        (n as u32).min(u16::MAX as u32) as u16
    } else {
        1
    }
}

/// Prefix-sum track start offsets into `out` (cleared first): track `i` starts
/// at the sum of tracks `0..i` plus `i` gaps. Final length = `sizes.len() + 1`
/// (the trailing entry is the end of the last track, used for span math).
/// Writes into a caller-owned buffer so the grid scratch pool reuses its
/// allocation across frames rather than allocating a fresh `Vec` per call.
///
/// `collapsed_tail` marks a count of trailing tracks that are collapsed to zero
/// width (CSS `auto-fit`): the gap *before* each collapsed track is dropped too,
/// so a run of empty trailing tracks folds onto the used edge and the last
/// non-empty track's cell reaches the reclaimed extent.
fn prefix_offsets_into(out: &mut Vec<f32>, sizes: &[f32], gap: f32) {
    prefix_offsets_into_collapsed(out, sizes, gap, 0);
}

fn prefix_offsets_into_collapsed(
    out: &mut Vec<f32>,
    sizes: &[f32],
    gap: f32,
    collapsed_tail: usize,
) {
    out.clear();
    out.reserve(sizes.len() + 1);
    // The first collapsed trailing track index: gaps at or after it are dropped.
    let first_collapsed = sizes.len().saturating_sub(collapsed_tail);
    let mut acc = 0.0f32;
    for (i, &s) in sizes.iter().enumerate() {
        out.push(acc);
        acc += s;
        // Add the gap after track `i` only when a later track follows AND the
        // boundary is not into the collapsed tail (its gaps are reclaimed).
        if i + 1 < sizes.len() && i + 1 < first_collapsed {
            acc += gap;
        }
    }
    out.push(acc);
}

/// The far edge of a span starting at `start` covering `span` tracks: the start
/// offset of the track just past the span, minus the trailing gap (a span's
/// interior gaps belong to the cell, the gap after it does not).
fn span_end(offsets: &[f32], sizes: &[f32], start: u16, span: u16, gap: f32) -> f32 {
    let end_track = (start + span) as usize;
    // The final offset entry is the content end: it carries no trailing gap
    // (`prefix_offsets` omits the gap after the last track), so a span reaching
    // it ends there directly with nothing to subtract.
    if end_track >= offsets.len() - 1 {
        return *offsets.last().unwrap_or(&0.0);
    }
    // A span ending on an interior track: offsets[end_track] includes the gap
    // before end_track; subtract it so the cell's far edge sits at the end of
    // the last covered track, not into the following gap.
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
            Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 200.0,
            },
            &mut scratch,
        );
        assert_eq!(
            store.bounds(kids[0]),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0
            }
        );
        assert_eq!(
            store.bounds(kids[1]),
            Rect {
                x: 100.0,
                y: 0.0,
                w: 100.0,
                h: 100.0
            }
        );
        assert_eq!(
            store.bounds(kids[2]),
            Rect {
                x: 0.0,
                y: 100.0,
                w: 100.0,
                h: 100.0
            }
        );
        assert_eq!(
            store.bounds(kids[3]),
            Rect {
                x: 100.0,
                y: 100.0,
                w: 100.0,
                h: 100.0
            }
        );
    }

    #[test]
    fn a_minmax_column_clamps_its_content_and_leaves_the_rest_to_fr() {
        use crate::component::NodeStore;
        use crate::grid::{GridStyle, TrackSizing};
        // [Minmax(40, 120), Fr(1)] in a 400x100 box. The first cell holds a fixed
        // 300-wide child (natural 300 > max 120 → clamps to 120); the Fr track
        // takes the remaining 280. A second child (fill) rides the Fr track.
        let mut store = NodeStore::new();
        let grid = store.alloc_grid(GridStyle {
            columns: vec![TrackSizing::Minmax(40.0, 120.0), TrackSizing::Fr(1.0)],
            rows: vec![TrackSizing::Fixed(100.0)],
            size: Size::fixed(400.0, 100.0),
            ..Default::default()
        });
        let big = {
            let k = store.alloc_grid(GridStyle {
                size: Size::fixed(300.0, 40.0),
                ..Default::default()
            });
            store.arena_append_child(grid, k);
            k
        };
        let filler = {
            let k = store.alloc_grid(GridStyle {
                size: Size::fill(),
                ..Default::default()
            });
            store.arena_append_child(grid, k);
            k
        };
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, grid.index(), &mut scratch);
        crate::layout::layout(
            &mut store,
            grid.index(),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 400.0,
                h: 100.0,
            },
            &mut scratch,
        );
        // The Minmax column is clamped to 120; a Fixed-size child hugs top-left,
        // so `big` sits at x=0 with its own 300 width (the cell clamp sizes the
        // TRACK, not the child — the child keeps its requested extent).
        assert_eq!(store.bounds(big).x, 0.0);
        // The Fr track starts at 120 and fills the remaining 280.
        assert_eq!(
            store.bounds(filler),
            Rect {
                x: 120.0,
                y: 0.0,
                w: 280.0,
                h: 100.0
            }
        );
    }

    #[test]
    fn a_minmax_column_floors_a_small_content_at_its_min() {
        use crate::component::NodeStore;
        use crate::grid::{GridStyle, TrackSizing};
        // [Minmax(80, 200), Fr(1)] in a 400x100 box. A fixed 30-wide child in the
        // first cell (natural 30 < min 80 → the track floors at 80); the Fr track
        // takes 320.
        let mut store = NodeStore::new();
        let grid = store.alloc_grid(GridStyle {
            columns: vec![TrackSizing::Minmax(80.0, 200.0), TrackSizing::Fr(1.0)],
            rows: vec![TrackSizing::Fixed(100.0)],
            size: Size::fixed(400.0, 100.0),
            ..Default::default()
        });
        let _small = {
            let k = store.alloc_grid(GridStyle {
                size: Size::fixed(30.0, 40.0),
                ..Default::default()
            });
            store.arena_append_child(grid, k);
            k
        };
        let filler = {
            let k = store.alloc_grid(GridStyle {
                size: Size::fill(),
                ..Default::default()
            });
            store.arena_append_child(grid, k);
            k
        };
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, grid.index(), &mut scratch);
        crate::layout::layout(
            &mut store,
            grid.index(),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 400.0,
                h: 100.0,
            },
            &mut scratch,
        );
        assert_eq!(
            store.bounds(filler),
            Rect {
                x: 80.0,
                y: 0.0,
                w: 320.0,
                h: 100.0
            }
        );
    }

    #[test]
    fn a_span_2_item_widens_the_two_auto_columns_it_covers() {
        use crate::component::NodeStore;
        use crate::grid::{GridPlacement, GridStyle, TrackSizing};
        // Three columns [Auto, Auto, Fr(1)] in a 500x100 box. A span-2 item of
        // 300 wide covers the two Auto columns: with no span-1 baseline they were
        // 0, so the 300 (no gap) splits 150/150 into the two Auto tracks. The Fr
        // track then takes the remaining 500 - 300 = 200. A filler on the Fr track
        // proves the split: it must start at x=300 and be 200 wide.
        let mut store = NodeStore::new();
        let grid = store.alloc_grid(GridStyle {
            columns: vec![TrackSizing::Auto, TrackSizing::Auto, TrackSizing::Fr(1.0)],
            rows: vec![TrackSizing::Fixed(100.0)],
            size: Size::fixed(500.0, 100.0),
            ..Default::default()
        });
        let wide = {
            let k = store.alloc_grid(GridStyle {
                size: Size::fixed(300.0, 40.0),
                ..Default::default()
            });
            store.set_grid_placement(
                k,
                GridPlacement {
                    column: Some(0),
                    row: Some(0),
                    column_span: 2,
                    row_span: 1,
                },
            );
            store.arena_append_child(grid, k);
            k
        };
        let filler = {
            let k = store.alloc_grid(GridStyle {
                size: Size::fill(),
                ..Default::default()
            });
            store.set_grid_placement(
                k,
                GridPlacement {
                    column: Some(2),
                    row: Some(0),
                    column_span: 1,
                    row_span: 1,
                },
            );
            store.arena_append_child(grid, k);
            k
        };
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, grid.index(), &mut scratch);
        crate::layout::layout(
            &mut store,
            grid.index(),
            surface_local(500.0, 100.0),
            &mut scratch,
        );
        // The span-2 item's cell spans columns 0..2 → x=0, and its Fixed child
        // hugs top-left at x=0.
        assert_eq!(store.bounds(wide).x, 0.0);
        // The Fr track sits after the two 150-wide Auto columns and fills 200.
        assert_eq!(
            store.bounds(filler),
            Rect {
                x: 300.0,
                y: 0.0,
                w: 200.0,
                h: 100.0
            }
        );
    }

    /// A surface-local box at the origin with the given extent — the root bounds
    /// a caller hands `layout` for a top-level container.
    fn surface_local(w: f32, h: f32) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        }
    }

    /// Allocate a track-less grid node of a given size as a grid child. A grid
    /// node with no tracks measures to its own `Size` request and is placed by
    /// its parent's grid arm exactly like a sized leaf: a `Fill` size stretches
    /// to the cell, a `Fixed` size hugs top-left. This is the same leaf-stand-in
    /// the sibling placement test relies on.
    fn cell_child(store: &mut crate::component::NodeStore, size: Size) -> crate::NodeId {
        use crate::grid::GridStyle;
        store.alloc_grid(GridStyle {
            columns: Vec::new(),
            rows: Vec::new(),
            size,
            ..Default::default()
        })
    }

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
        let a = cell_child(&mut store, Size::fill());
        let b = cell_child(&mut store, Size::fill());
        store.arena_append_child(grid, a);
        store.arena_append_child(grid, b);
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, grid.index(), &mut scratch);
        crate::layout::layout(
            &mut store,
            grid.index(),
            surface_local(250.0, 70.0),
            &mut scratch,
        );
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
        let c = cell_child(&mut store, Size::fixed(30.0, 40.0));
        store.arena_append_child(grid, c);
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, grid.index(), &mut scratch);
        crate::layout::layout(
            &mut store,
            grid.index(),
            surface_local(100.0, 100.0),
            &mut scratch,
        );
        assert_eq!(
            store.bounds(c),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 30.0,
                h: 40.0
            }
        );
    }

    /// A `Fit` text leaf of a given intrinsic height and first-line baseline,
    /// appended as a grid child. It measures to `natural.y` on the block axis and
    /// reports `baseline` through `content_baseline`, so a row's baseline math is
    /// exercised with real per-child values. The glyph run and atlas are empty —
    /// only the content's `natural`/`baseline` fields matter to layout.
    fn text_cell_child(
        store: &mut crate::component::NodeStore,
        height: f32,
        baseline: f32,
    ) -> crate::NodeId {
        use crate::content::Content;
        use viso_render::{Rgba, TextureId};
        let id = store.alloc_leaf(Size {
            width: Length::Fit,
            height: Length::Fit,
        });
        store.set_content_payload(
            id,
            Content::Text {
                glyphs: Vec::new(),
                atlas: TextureId(0),
                color: Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                natural: Vec2 { x: 20.0, y: height },
                baseline,
            },
        );
        id
    }

    #[test]
    fn baseline_align_seats_mixed_cells_on_one_baseline() {
        use crate::component::NodeStore;
        use crate::grid::{GridStyle, TrackSizing};
        // One row, two auto-flow columns, 100px tall row. Two text cells of
        // different heights and different first-line baselines. With
        // `AlignItems::Baseline` each child's own baseline lands on the row's
        // shared baseline (the max of the two), so their baselines coincide.
        let mut store = NodeStore::new();
        let grid = store.alloc_grid(GridStyle {
            columns: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
            rows: vec![TrackSizing::Fixed(100.0)],
            align_items: AlignItems::Baseline,
            size: Size::fixed(100.0, 100.0),
            ..Default::default()
        });
        // Tall cell: 40px run, baseline 30 below its top.
        let tall = text_cell_child(&mut store, 40.0, 30.0);
        // Short cell: 24px run, baseline 18 below its top.
        let short = text_cell_child(&mut store, 24.0, 18.0);
        store.arena_append_child(grid, tall);
        store.arena_append_child(grid, short);
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, grid.index(), &mut scratch);
        crate::layout::layout(
            &mut store,
            grid.index(),
            surface_local(100.0, 100.0),
            &mut scratch,
        );
        // Shared baseline = max(30, 18) = 30, measured from the row's top (y=0).
        // Tall child sits at its full baseline (dy = 30 - 30 = 0) → top y = 0,
        // its baseline at 0 + 30 = 30.
        let tb = store.bounds(tall);
        assert_eq!(
            tb.y, 0.0,
            "tall cell anchors the shared baseline at its top"
        );
        // Short child offsets down (dy = 30 - 18 = 12) → top y = 12, its baseline
        // at 12 + 18 = 30. Both baselines coincide at y = 30.
        let sb = store.bounds(short);
        assert_eq!(
            sb.y, 12.0,
            "short cell drops so its baseline meets the row's"
        );
        assert_eq!(
            tb.y + 30.0,
            sb.y + 18.0,
            "both cells' first-line baselines land on the same y"
        );
    }

    #[test]
    fn align_items_offsets_a_fixed_cell_in_its_row() {
        use crate::component::NodeStore;
        use crate::grid::{GridStyle, TrackSizing};
        // A 40px-tall fixed child in a 100px row, laid out under each non-baseline
        // align_items. Start hugs the top, Center splits the slack, End drops to
        // the bottom; Stretch (the default) still hugs its own height for a Fixed
        // child (only Fill children grow), so it matches Start.
        let cases = [
            (AlignItems::Start, 0.0),
            (AlignItems::Center, 30.0),
            (AlignItems::End, 60.0),
            (AlignItems::Stretch, 0.0),
        ];
        for (align, want_y) in cases {
            let mut store = NodeStore::new();
            let grid = store.alloc_grid(GridStyle {
                columns: vec![TrackSizing::Fixed(50.0)],
                rows: vec![TrackSizing::Fixed(100.0)],
                align_items: align,
                size: Size::fixed(50.0, 100.0),
                ..Default::default()
            });
            let child = cell_child(&mut store, Size::fixed(30.0, 40.0));
            store.arena_append_child(grid, child);
            let mut scratch = Vec::new();
            crate::layout::measure(&mut store, grid.index(), &mut scratch);
            crate::layout::layout(
                &mut store,
                grid.index(),
                surface_local(50.0, 100.0),
                &mut scratch,
            );
            let b = store.bounds(child);
            assert_eq!(b.h, 40.0, "{align:?}: a fixed child keeps its own height");
            assert_eq!(b.y, want_y, "{align:?}: child block offset within the row");
        }
    }

    #[test]
    fn align_items_stretch_grows_a_fill_child_to_the_cell() {
        use crate::component::NodeStore;
        use crate::grid::{GridStyle, TrackSizing};
        // A Fill child stretches to the cell only under the default Stretch; any
        // other align_items makes it hug its measured height (0 here) and seats it
        // per the alignment instead of filling.
        for (align, want_h, want_y) in [
            (AlignItems::Stretch, 100.0, 0.0),
            (AlignItems::Start, 0.0, 0.0),
            (AlignItems::End, 0.0, 100.0),
        ] {
            let mut store = NodeStore::new();
            let grid = store.alloc_grid(GridStyle {
                columns: vec![TrackSizing::Fixed(50.0)],
                rows: vec![TrackSizing::Fixed(100.0)],
                align_items: align,
                size: Size::fixed(50.0, 100.0),
                ..Default::default()
            });
            let child = cell_child(&mut store, Size::fill());
            store.arena_append_child(grid, child);
            let mut scratch = Vec::new();
            crate::layout::measure(&mut store, grid.index(), &mut scratch);
            crate::layout::layout(
                &mut store,
                grid.index(),
                surface_local(50.0, 100.0),
                &mut scratch,
            );
            let b = store.bounds(child);
            assert_eq!(b.h, want_h, "{align:?}: fill child block extent");
            assert_eq!(b.y, want_y, "{align:?}: fill child block offset");
        }
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
            let k = cell_child(&mut store, Size::fill());
            store.arena_append_child(grid, k);
            kids.push(k);
        }
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, grid.index(), &mut scratch);
        crate::layout::layout(
            &mut store,
            grid.index(),
            surface_local(100.0, 120.0),
            &mut scratch,
        );
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
            let k = cell_child(&mut store, Size::fill());
            store.arena_append_child(grid, k);
        }
        let mut scratch = Vec::new();
        // Warm up, then assert the shared scratch capacity is stable across frames.
        crate::layout::layout(
            &mut store,
            grid.index(),
            surface_local(200.0, 200.0),
            &mut scratch,
        );
        let cap = scratch.capacity();
        for _ in 0..50 {
            crate::layout::layout(
                &mut store,
                grid.index(),
                surface_local(200.0, 200.0),
                &mut scratch,
            );
        }
        assert_eq!(
            scratch.capacity(),
            cap,
            "shared layout scratch must not grow per frame"
        );
    }

    #[test]
    fn a_column_subgrid_adopts_the_parents_column_lines_including_gaps() {
        use crate::component::NodeStore;
        use crate::grid::{GridPlacement, GridStyle, TrackSizing};
        // Parent: 3 columns [100, 60, Fr(1)] with a 20 column gap in a 400x100
        // box. Column lines fall at x = 0, 100, (120..180), (200..400). A
        // subgrid child placed at column 0 spanning all 3 columns must reproduce
        // those exact interior lines for its own two children, gaps included:
        // it does NOT re-solve — it adopts the parent's [100, 60, 200] widths and
        // the parent's 20 gap. The child spans row 0 (self-solved, one Fixed row).
        let mut store = NodeStore::new();
        let parent = store.alloc_grid(GridStyle {
            columns: vec![
                TrackSizing::Fixed(100.0),
                TrackSizing::Fixed(60.0),
                TrackSizing::Fr(1.0),
            ],
            rows: vec![TrackSizing::Fixed(100.0)],
            column_gap: 20.0,
            size: Size::fixed(400.0, 100.0),
            ..Default::default()
        });
        // The subgrid child itself spans the parent's 3 columns; on the column
        // axis it is a subgrid, so its own (empty) column template is ignored.
        let sub = store.alloc_grid(GridStyle {
            // A deliberately different local gap and bogus templates prove the
            // subgrid axis ignores them and inherits the parent's tracks + gap.
            columns: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
            rows: vec![TrackSizing::Fixed(100.0)],
            column_gap: 5.0,
            subgrid_columns: true,
            size: Size::fill(),
            ..Default::default()
        });
        store.set_grid_placement(
            sub,
            GridPlacement {
                column: Some(0),
                row: Some(0),
                column_span: 3,
                row_span: 1,
            },
        );
        store.arena_append_child(parent, sub);
        // Two children of the subgrid: one on inherited column 0, one on column 2.
        let a = cell_child(&mut store, Size::fill());
        store.set_grid_placement(
            a,
            GridPlacement {
                column: Some(0),
                row: Some(0),
                column_span: 1,
                row_span: 1,
            },
        );
        store.arena_append_child(sub, a);
        let b = cell_child(&mut store, Size::fill());
        store.set_grid_placement(
            b,
            GridPlacement {
                column: Some(2),
                row: Some(0),
                column_span: 1,
                row_span: 1,
            },
        );
        store.arena_append_child(sub, b);

        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, parent.index(), &mut scratch);
        crate::layout::layout(
            &mut store,
            parent.index(),
            surface_local(400.0, 100.0),
            &mut scratch,
        );

        // The subgrid box spans the full parent width.
        assert_eq!(store.bounds(sub), surface_local(400.0, 100.0));
        // Child a lands on the parent's column 0 (x=0, w=100).
        assert_eq!(
            store.bounds(a),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0
            }
        );
        // Child b lands on the parent's column 2: line at 100 + 20 + 60 + 20 = 200,
        // width 400 - 200 = 200 (Fr). Its left edge coincides with the parent's
        // third column line, proving gap-inclusive line coincidence.
        assert_eq!(
            store.bounds(b),
            Rect {
                x: 200.0,
                y: 0.0,
                w: 200.0,
                h: 100.0
            }
        );
    }

    #[test]
    fn a_span_offset_subgrid_adopts_only_the_parent_tracks_it_covers() {
        use crate::component::NodeStore;
        use crate::grid::{GridPlacement, GridStyle, TrackSizing};
        // Parent: 4 columns [50, 80, 120, Fr(1)] no gap in a 400x100 box. Lines at
        // x = 0, 50, 130, 250, 400. A subgrid child placed at column 1 spanning 2
        // columns adopts the parent's columns 1..3 → widths [80, 120]. Its two
        // children must land at the subgrid-local origin (x=130) with widths 80
        // and 120 — i.e. the parent's k..k+n segment, not columns 0.. .
        let mut store = NodeStore::new();
        let parent = store.alloc_grid(GridStyle {
            columns: vec![
                TrackSizing::Fixed(50.0),
                TrackSizing::Fixed(80.0),
                TrackSizing::Fixed(120.0),
                TrackSizing::Fr(1.0),
            ],
            rows: vec![TrackSizing::Fixed(100.0)],
            size: Size::fixed(400.0, 100.0),
            ..Default::default()
        });
        let sub = store.alloc_grid(GridStyle {
            rows: vec![TrackSizing::Fixed(100.0)],
            subgrid_columns: true,
            size: Size::fill(),
            ..Default::default()
        });
        store.set_grid_placement(
            sub,
            GridPlacement {
                column: Some(1),
                row: Some(0),
                column_span: 2,
                row_span: 1,
            },
        );
        store.arena_append_child(parent, sub);
        let a = cell_child(&mut store, Size::fill());
        store.arena_append_child(sub, a);
        let b = cell_child(&mut store, Size::fill());
        store.arena_append_child(sub, b);

        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, parent.index(), &mut scratch);
        crate::layout::layout(
            &mut store,
            parent.index(),
            surface_local(400.0, 100.0),
            &mut scratch,
        );

        // The subgrid box covers parent columns 1..3: [50, 250) → x=50, w=200.
        assert_eq!(
            store.bounds(sub),
            Rect {
                x: 50.0,
                y: 0.0,
                w: 200.0,
                h: 100.0
            }
        );
        // Auto-flow: a on inherited column 0 (parent col 1, width 80) at x=50,
        // b on inherited column 1 (parent col 2, width 120) at x=130.
        assert_eq!(
            store.bounds(a),
            Rect {
                x: 50.0,
                y: 0.0,
                w: 80.0,
                h: 100.0
            }
        );
        assert_eq!(
            store.bounds(b),
            Rect {
                x: 130.0,
                y: 0.0,
                w: 120.0,
                h: 100.0
            }
        );
    }

    #[test]
    fn a_both_axis_subgrid_adopts_parent_lines_and_a_mixed_axis_self_solves_rows() {
        use crate::component::NodeStore;
        use crate::grid::{GridPlacement, GridStyle, TrackSizing};
        // Parent: 2x2 columns [100, Fr(1)], rows [40, Fr(1)] no gap in 300x200.
        // Column lines: 0, 100, 300. Row lines: 0, 40, 200.
        // (1) A both-axis subgrid spanning the whole parent reproduces all four
        //     inner cell corners on the parent's lines.
        // (2) The same subgrid's rows would, if self-solved, differ — so this
        //     also guards that subgrid_rows is honored (not silently self-solved).
        let mut store = NodeStore::new();
        let parent = store.alloc_grid(GridStyle {
            columns: vec![TrackSizing::Fixed(100.0), TrackSizing::Fr(1.0)],
            rows: vec![TrackSizing::Fixed(40.0), TrackSizing::Fr(1.0)],
            size: Size::fixed(300.0, 200.0),
            ..Default::default()
        });
        let sub = store.alloc_grid(GridStyle {
            // Bogus local templates: both axes are subgrids, so both are ignored.
            columns: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
            rows: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
            subgrid_columns: true,
            subgrid_rows: true,
            size: Size::fill(),
            ..Default::default()
        });
        store.set_grid_placement(
            sub,
            GridPlacement {
                column: Some(0),
                row: Some(0),
                column_span: 2,
                row_span: 2,
            },
        );
        store.arena_append_child(parent, sub);
        // Four auto-flow children fill the subgrid's inherited 2x2 lattice.
        let mut kids = Vec::new();
        for _ in 0..4 {
            let k = cell_child(&mut store, Size::fill());
            store.arena_append_child(sub, k);
            kids.push(k);
        }

        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, parent.index(), &mut scratch);
        crate::layout::layout(
            &mut store,
            parent.index(),
            surface_local(300.0, 200.0),
            &mut scratch,
        );

        // Cells land on the parent's exact column lines (0/100/300) and row lines
        // (0/40/200): a self-solved Fr row would have split 200 evenly (100/100),
        // so a top-left of 40 for the second row proves rows were inherited.
        assert_eq!(
            store.bounds(kids[0]),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 40.0
            }
        );
        assert_eq!(
            store.bounds(kids[1]),
            Rect {
                x: 100.0,
                y: 0.0,
                w: 200.0,
                h: 40.0
            }
        );
        assert_eq!(
            store.bounds(kids[2]),
            Rect {
                x: 0.0,
                y: 40.0,
                w: 100.0,
                h: 160.0
            }
        );
        assert_eq!(
            store.bounds(kids[3]),
            Rect {
                x: 100.0,
                y: 40.0,
                w: 200.0,
                h: 160.0
            }
        );
    }

    #[test]
    fn a_mixed_axis_subgrid_inherits_columns_and_self_solves_rows() {
        use crate::component::NodeStore;
        use crate::grid::{GridPlacement, GridStyle, TrackSizing};
        // Parent: columns [100, Fr(1)] rows [Fixed(200)] in 300x200. A subgrid
        // child spans both columns (subgrid) but keeps its OWN rows: two Fr rows.
        // Columns therefore land on the parent's lines (0/100/300), while rows
        // split the child's 200 height evenly (100/100) — proving one axis
        // inherits and the other self-solves in the same node.
        let mut store = NodeStore::new();
        let parent = store.alloc_grid(GridStyle {
            columns: vec![TrackSizing::Fixed(100.0), TrackSizing::Fr(1.0)],
            rows: vec![TrackSizing::Fixed(200.0)],
            size: Size::fixed(300.0, 200.0),
            ..Default::default()
        });
        let sub = store.alloc_grid(GridStyle {
            rows: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
            subgrid_columns: true,
            size: Size::fill(),
            ..Default::default()
        });
        store.set_grid_placement(
            sub,
            GridPlacement {
                column: Some(0),
                row: Some(0),
                column_span: 2,
                row_span: 1,
            },
        );
        store.arena_append_child(parent, sub);
        // Two children on the same inherited column 0 but different self-solved
        // rows (auto-flow wraps to the next row after column 1 is unused here:
        // pin them explicitly to rows 0 and 1 of column 0).
        let top = cell_child(&mut store, Size::fill());
        store.set_grid_placement(
            top,
            GridPlacement {
                column: Some(0),
                row: Some(0),
                column_span: 1,
                row_span: 1,
            },
        );
        store.arena_append_child(sub, top);
        let bottom = cell_child(&mut store, Size::fill());
        store.set_grid_placement(
            bottom,
            GridPlacement {
                column: Some(0),
                row: Some(1),
                column_span: 1,
                row_span: 1,
            },
        );
        store.arena_append_child(sub, bottom);

        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, parent.index(), &mut scratch);
        crate::layout::layout(
            &mut store,
            parent.index(),
            surface_local(300.0, 200.0),
            &mut scratch,
        );

        // Inherited column 0 (x=0, w=100); self-solved rows split 200 → 100/100.
        assert_eq!(
            store.bounds(top),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0
            }
        );
        assert_eq!(
            store.bounds(bottom),
            Rect {
                x: 0.0,
                y: 100.0,
                w: 100.0,
                h: 100.0
            }
        );
    }

    #[test]
    fn a_hidden_subtree_measures_zero_and_contributes_nothing_to_the_parent() {
        use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
        use crate::style::BoxStyle;

        // A row of two 40x40 leaves. Hiding the second must fold it out of the
        // row's main-axis sum: the row measures to one leaf, not two.
        let mut store = NodeStore::new();
        let mut hidden_leaf = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    ..Default::default()
                },
                |cx| {
                    cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 40.0),
                        style: BoxStyle::NONE,
                    });
                    let h = cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 40.0),
                        style: BoxStyle::NONE,
                    });
                    hidden_leaf = Some(h.id());
                },
            );
            cx.root().unwrap()
        };
        let hidden_leaf = hidden_leaf.unwrap();
        let mut scratch = Vec::new();

        // Shown: the row's natural main extent spans both leaves.
        measure(&mut store, root.index(), &mut scratch);
        assert_eq!(LayoutTree::measured(&store, root.index()).w, 80.0);

        // Hide the second leaf: it measures to zero and drops out of the sum.
        store.set_hidden(hidden_leaf, true);
        measure(&mut store, root.index(), &mut scratch);
        assert_eq!(
            LayoutTree::measured(&store, hidden_leaf.index()),
            Measured { w: 0.0, h: 0.0 },
            "a hidden subtree measures to zero"
        );
        assert_eq!(
            LayoutTree::measured(&store, root.index()).w,
            40.0,
            "the hidden leaf contributes nothing to the row's main sum"
        );
    }

    #[test]
    fn a_hidden_fill_subtree_lays_out_to_a_zero_rect_without_panicking() {
        use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
        use crate::style::BoxStyle;

        // A column with a single fill leaf. Hidden, its layout must early-return
        // to a zero rect at the parent origin — never dividing free space among
        // zero visible weights.
        let mut store = NodeStore::new();
        let mut fill_leaf = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    ..Default::default()
                },
                |cx| {
                    let h = cx.leaf(LeafStyle {
                        size: Size::fill(),
                        style: BoxStyle::NONE,
                    });
                    fill_leaf = Some(h.id());
                },
            );
            cx.root().unwrap()
        };
        let fill_leaf = fill_leaf.unwrap();
        store.set_hidden(fill_leaf, true);

        let mut scratch = Vec::new();
        measure(&mut store, root.index(), &mut scratch);
        layout(
            &mut store,
            root.index(),
            surface_local(100.0, 100.0),
            &mut scratch,
        );

        let b = store.bounds(fill_leaf);
        assert_eq!(b.w, 0.0);
        assert_eq!(b.h, 0.0);
    }

    #[test]
    fn adaptive_column_count_fits_by_min_width_and_gap() {
        // No gap: floor(width / min), at least one.
        assert_eq!(adaptive_column_count(200.0, 0.0, 100.0), 1); // narrower than one min
        assert_eq!(adaptive_column_count(200.0, 0.0, 200.0), 1); // exactly one
        assert_eq!(adaptive_column_count(200.0, 0.0, 850.0), 4); // floor(4.25)
        // With a gap, `n` tracks carry `n-1` gaps: floor((w + gap) / (min + gap)).
        // 3×200 + 2×20 = 640 fits in 640; a fourth would need 860.
        assert_eq!(adaptive_column_count(200.0, 20.0, 640.0), 3);
        assert_eq!(adaptive_column_count(200.0, 20.0, 859.0), 3);
        assert_eq!(adaptive_column_count(200.0, 20.0, 860.0), 4);
        // Degenerate min: one column, never a divide-by-zero.
        assert_eq!(adaptive_column_count(0.0, 0.0, 500.0), 1);
    }

    /// Build a grid whose children are fill leaves and lay it out at `w`x`h`,
    /// returning the store and child ids so a golden can read cell rects.
    fn adaptive_grid(
        adaptive: AdaptiveColumns,
        child_count: usize,
        column_gap: f32,
        w: f32,
        h: f32,
    ) -> (crate::component::NodeStore, Vec<crate::node::NodeId>) {
        use crate::component::NodeStore;
        use crate::grid::{GridStyle, TrackSizing};
        let mut store = NodeStore::new();
        let grid = store.alloc_grid(GridStyle {
            columns: Vec::new(),
            rows: vec![TrackSizing::Fixed(50.0)],
            adaptive_columns: Some(adaptive),
            column_gap,
            size: Size::fixed(w, h),
            ..Default::default()
        });
        let mut kids = Vec::new();
        for _ in 0..child_count {
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
        measure(&mut store, grid.index(), &mut scratch);
        layout(
            &mut store,
            grid.index(),
            Rect {
                x: 0.0,
                y: 0.0,
                w,
                h,
            },
            &mut scratch,
        );
        (store, kids)
    }

    #[test]
    fn adaptive_fill_scales_column_count_with_container_width() {
        // minmax(100, 1fr), auto-fill. Narrow (150) → 1 column stretched to 150.
        let a = AdaptiveColumns::auto_fill(100.0, TrackMax::Fr(1.0));
        let (store, kids) = adaptive_grid(a, 3, 0.0, 150.0, 50.0);
        let b0 = store.bounds(kids[0]);
        assert_eq!(b0.x, 0.0);
        assert_eq!(b0.w, 150.0);
        // The other two children wrap onto implicit rows in the single column.
        assert_eq!(store.bounds(kids[1]).x, 0.0);

        // Wide (600) → 6 columns of 100, each child in its own column on row 0.
        let (store, kids) = adaptive_grid(a, 3, 0.0, 600.0, 50.0);
        assert_eq!(store.bounds(kids[0]).x, 0.0);
        assert_eq!(store.bounds(kids[0]).w, 100.0);
        assert_eq!(store.bounds(kids[1]).x, 100.0);
        assert_eq!(store.bounds(kids[2]).x, 200.0);
        assert_eq!(store.bounds(kids[2]).y, 0.0);
    }

    #[test]
    fn adaptive_fill_minmax_1fr_stretches_columns_to_fill_width() {
        // minmax(200, 1fr) in 500px → floor(500/200) = 2 columns, each stretched to
        // 250 (min 200 + 50 leftover share), so the row exactly fills the width.
        let a = AdaptiveColumns::auto_fill(200.0, TrackMax::Fr(1.0));
        let (store, kids) = adaptive_grid(a, 2, 0.0, 500.0, 50.0);
        assert_eq!(store.bounds(kids[0]).x, 0.0);
        assert_eq!(store.bounds(kids[0]).w, 250.0);
        assert_eq!(store.bounds(kids[1]).x, 250.0);
        assert_eq!(store.bounds(kids[1]).w, 250.0);
    }

    #[test]
    fn adaptive_fill_px_max_leaves_the_remainder_free() {
        // minmax(100, 150px) in 500px → 5 columns; content is a fill leaf (0 natural
        // width) so each track clamps to its 100 min, not the 150 cap. The trailing
        // columns stay open (auto-fill), so a 2-child grid still has 5 columns and
        // the second child lands at x=100.
        let a = AdaptiveColumns::auto_fill(100.0, TrackMax::Px(150.0));
        let (store, kids) = adaptive_grid(a, 2, 0.0, 500.0, 50.0);
        assert_eq!(store.bounds(kids[0]).w, 100.0);
        assert_eq!(store.bounds(kids[1]).x, 100.0);
    }

    #[test]
    fn adaptive_fit_collapses_trailing_empty_columns() {
        // minmax(100, 1fr), auto-fit in 600px → 6 computed columns. Only 2 children,
        // so columns 2..6 are empty and collapse to zero width; their free-space
        // share is reclaimed by the two occupied columns, which split the whole
        // 600px → 300 each. (auto-fill would instead keep six 100px columns.)
        let a = AdaptiveColumns::auto_fit(100.0, TrackMax::Fr(1.0));
        let (store, kids) = adaptive_grid(a, 2, 0.0, 600.0, 50.0);
        assert_eq!(store.bounds(kids[0]).x, 0.0);
        assert_eq!(store.bounds(kids[0]).w, 300.0);
        assert_eq!(store.bounds(kids[1]).x, 300.0);
        assert_eq!(store.bounds(kids[1]).w, 300.0);
        // Right edge of the last occupied cell = the container's content width.
        let b1 = store.bounds(kids[1]);
        assert_eq!(b1.x + b1.w, 600.0);
    }

    #[test]
    fn adaptive_fill_keeps_trailing_empty_columns() {
        // Same shape as the auto-fit case but auto-fill: the six 100px columns all
        // stay, so the two children hold their 100px cells at x=0 and x=100 and the
        // trailing four columns keep their width (row does not collapse).
        let a = AdaptiveColumns::auto_fill(100.0, TrackMax::Fr(1.0));
        let (store, kids) = adaptive_grid(a, 2, 0.0, 600.0, 50.0);
        assert_eq!(store.bounds(kids[0]).w, 100.0);
        assert_eq!(store.bounds(kids[1]).x, 100.0);
        assert_eq!(store.bounds(kids[1]).w, 100.0);
    }
}
