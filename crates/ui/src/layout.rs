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
use crate::length::LengthIssue;
use viso_render::Rect;

pub use viso_math::Vec2;
pub use viso_schema::layout::{
    AbsoluteInsets, Align, AlignItems, Alignment2D, Axis, BoxRules, Inset, Justify, Length,
    Placement, Span, SpanInset, StackPlacement,
};

/// A [`Vec2`]'s component along a layout [`Axis`] (x for Row, y for Column).
pub trait OnAxis {
    /// The component along `axis`.
    fn on(self, axis: Axis) -> f32;
}

impl OnAxis for Vec2 {
    #[inline]
    fn on(self, axis: Axis) -> f32 {
        match axis {
            Axis::Row => self.x,
            Axis::Column => self.y,
        }
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
    /// An exact dp box.
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
        /// Main-axis placement of the packed child group.
        justify: Justify,
        /// The container's own size request within its parent.
        size: Size,
    },
    /// A container that layers its children in its content box: each sits
    /// where its own [`StackPlacement::align`] or the stack's `align` puts it,
    /// and the stack fits the largest.
    Stack {
        /// Inner padding on all four edges.
        padding: Inset,
        /// Where a child without its own alignment sits.
        align: Alignment2D,
        /// The container's own size request within its parent.
        size: Size,
    },
    /// A container that pins each child by its [`AbsoluteInsets`] to the
    /// edges of its content box. No child enters into its size.
    Absolute {
        /// Inner padding on all four edges.
        padding: Inset,
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
        /// The room across the axis kept for a classic scroll bar, at the
        /// viewport's end (its start, for a column read right to left); the
        /// content fills the rest.
        gutter: f32,
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
            | LayoutInput::Stack { size, .. }
            | LayoutInput::Absolute { size, .. }
            | LayoutInput::Leaf { size }
            | LayoutInput::Scroll { size, .. }
            | LayoutInput::AbsoluteRows { size, .. }
            | LayoutInput::Grid { size, .. } => size,
        }
    }

    /// A mutable borrow of the node's own size request, for a reconcile step that
    /// rewrites a live node's length in place (see
    /// [`NodeStore::set_flex_child_weight`](crate::NodeStore::set_flex_child_weight)).
    #[inline]
    pub fn size_mut(&mut self) -> &mut Size {
        match self {
            LayoutInput::Flex { size, .. }
            | LayoutInput::Stack { size, .. }
            | LayoutInput::Absolute { size, .. }
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
    /// The narrowest width it lays out in without overflowing — its widest
    /// text stretch no line breaks inside — which a `fit` width shrinks no
    /// further than. Never wider than `w`.
    pub min_w: f32,
}

impl Measured {
    /// What a node requesting `size` measures to over content of natural
    /// extent `content` and min-content width `min`: a fixed request its
    /// own, `MinContent` the narrowest width and `MaxContent` the widest on
    /// both counts, any other the content's.
    #[inline]
    fn sized(size: Size, content: Vec2, min: f32) -> Self {
        let (w, min_w) = match size.width {
            Length::Fixed(v) => (v, v),
            Length::MinContent => (min, min),
            Length::MaxContent => (content.x, content.x),
            _ => (content.x, min.min(content.x)),
        };
        let h = match size.height {
            Length::Fixed(v) => v,
            _ => content.y,
        };
        Self { w, h, min_w }
    }

    /// The extent a `fit` request takes along `axis` in `room`: as wide as
    /// the room allows down to the min-content width; a height is the
    /// content's, which only the width moves.
    #[inline]
    fn fit(self, axis: Axis, room: f32) -> f32 {
        match axis {
            Axis::Row => self.w.min(room.max(self.min_w)),
            Axis::Column => self.h,
        }
    }

    /// The natural extent along a given axis.
    #[inline]
    fn on(self, axis: Axis) -> f32 {
        match axis {
            Axis::Row => self.w,
            Axis::Column => self.h,
        }
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
    /// The column and row gaps' percentages (fractions) of a grid's content
    /// box.
    fn grid_gap_pct(&self, _index: u32) -> [f32; 2] {
        [0.0; 2]
    }
    /// Where a child sits in its parent beyond its size, or `None` for a
    /// child that says nothing of it.
    fn placement(&self, index: u32) -> Option<&Placement>;
    /// A child's placement inside its grid parent (default = auto-flow, span 1).
    #[inline]
    fn grid_placement(&self, index: u32) -> GridPlacement {
        match self.placement(index) {
            Some(Placement::Grid(grid)) => *grid,
            _ => GridPlacement::default(),
        }
    }
    /// A node's content intrinsic size, or `None` when it carries no drawable
    /// content. A `Fit`/`Fill` leaf axis resolves against this; a `None` leaf
    /// measures to `0` on both axes (a bare layout leaf, the prior behavior).
    fn content_natural(&self, index: u32) -> Option<Vec2>;
    /// The narrowest width a node's content lays out in without overflowing,
    /// read only when it has content: a text's widest stretch no line breaks
    /// inside. Its natural width by default, content that does not wrap.
    fn content_min_width(&self, index: u32) -> f32 {
        self.content_natural(index).map_or(0.0, |natural| natural.x)
    }
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
    /// The rules bounding a node's box beyond its size request — its least and
    /// greatest extents, its margin, its padding percentage — or `None` for a
    /// node with none, the common case.
    fn rules(&self, index: u32) -> Option<&BoxRules> {
        let _ = index;
        None
    }
    /// Record the parent content box `within` (NaN where indefinite) a
    /// container with rules was laid in, so a re-layout of it alone reads the
    /// same percentages, and give its rules; [`kept_within`](Self::kept_within)
    /// gives the content box back. Keeps nothing by default.
    fn keep_within(&mut self, index: u32, within: Vec2) -> Option<&BoxRules> {
        let _ = (index, within);
        None
    }
    /// The content box [`keep_within`](Self::keep_within) recorded for a
    /// container with rules; indefinite for any other.
    fn kept_within(&self, index: u32) -> Vec2 {
        let _ = index;
        INDEFINITE
    }
    /// Whether lines run right to left in container `index`: a row places
    /// its children from the right, `start` and `end` alignments and insets
    /// name the right and left edges, grid columns run from the right and a
    /// row's scroll content starts at its right edge. Left to right by
    /// default.
    fn rtl(&self, index: u32) -> bool {
        let _ = index;
        false
    }
    /// Whether a node is its scroll viewport's chrome (a scroll bar track),
    /// laid out along the viewport's edge rather than as its content.
    fn chrome(&self, index: u32) -> bool {
        let _ = index;
        false
    }
    /// Whether a node (and its subtree) is folded out of layout. A hidden node
    /// measures to zero on both axes and lays out to a zero rect, contributing
    /// nothing to a parent's main-axis sum, without a structural rebuild.
    fn hidden(&self, index: u32) -> bool;
    /// Whether a node is laid out apart from its parent, which measures and
    /// places its other children as if it were not there: a layer over the
    /// whole surface.
    fn out_of_flow(&self, _index: u32) -> bool {
        false
    }
    /// Record that a child was assigned `width` (physical px) by its Flex
    /// parent, so a wrap-eligible text leaf can be reshaped at that width in a
    /// later pass. A pure data write: the implementation decides eligibility
    /// (text content, width `Fill`/`Fixed`, `soft_wrap`, width actually changed)
    /// and drops the request otherwise. The layout engine calls it
    /// unconditionally for every Flex child; it never reads text back, so the
    /// measure/layout passes stay ignorant of shaping. A no-op by default so
    /// containers that do not opt into reflow (and non-store `LayoutTree`
    /// impls) need not implement it.
    fn request_text_reflow(&mut self, index: u32, width: f32) {
        let _ = (index, width);
    }
    /// Record that node `index`'s length could not resolve as authored: a
    /// percent term met an indefinite basis. A no-op by default.
    fn report_length(&mut self, index: u32, issue: LengthIssue) {
        let _ = (index, issue);
    }
    /// A set of grid layout working buffers, from the tree's pool when it
    /// keeps one; [`put_grid_scratch`](Self::put_grid_scratch) gives it back.
    /// A fresh set by default, so a tree that keeps no pool allocates per
    /// grid laid out.
    fn take_grid_scratch(&mut self) -> GridScratch {
        GridScratch::default()
    }
    /// Gives back a set [`take_grid_scratch`](Self::take_grid_scratch)
    /// handed out. Dropped by default.
    fn put_grid_scratch(&mut self, scratch: GridScratch) {
        let _ = scratch;
    }
}

/// Whether a box is a definite percent basis on each axis: its extent does not
/// come from its own content. The root's box is definite; a box that fits its
/// content, and a scroll content's scrolling axis, are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Basis {
    /// The horizontal extent is definite.
    pub width: bool,
    /// The vertical extent is definite.
    pub height: bool,
}

impl Basis {
    /// Definite on both axes, as the root is.
    pub const DEFINITE: Self = Self {
        width: true,
        height: true,
    };

    #[inline]
    fn on(self, axis: Axis) -> bool {
        match axis {
            Axis::Row => self.width,
            Axis::Column => self.height,
        }
    }

    #[inline]
    fn pack(axis: Axis, main: bool, cross: bool) -> Self {
        match axis {
            Axis::Row => Self {
                width: main,
                height: cross,
            },
            Axis::Column => Self {
                width: cross,
                height: main,
            },
        }
    }

    /// The basis a child of a box on this basis lays out its own children on.
    /// `parent` is the box's layout input, `child` the child's.
    pub fn child(self, parent: LayoutInput, child: LayoutInput) -> Self {
        let size = child.size();
        match parent {
            LayoutInput::Flex { axis, align, .. } => {
                let main = self.on(axis);
                let cross = self.on(cross_of(axis));
                let main = match size.on(axis) {
                    Length::Fixed(_) => true,
                    Length::Relative { fixed, .. } => main || fixed != 0.0,
                    Length::Fill { .. } => main,
                    Length::Fit | Length::MinContent | Length::MaxContent => false,
                };
                let cross = match size.cross(axis) {
                    Length::Fixed(_) => true,
                    Length::Relative { fixed, .. } => cross || fixed != 0.0,
                    Length::Fill { .. } => cross,
                    Length::Fit => cross && align == Align::Stretch,
                    Length::MinContent | Length::MaxContent => false,
                };
                Self::pack(axis, main, cross)
            }
            // An absolute child pinned to both edges of an axis lays out on
            // what `placed` gives it; its parent's pass passes that.
            LayoutInput::Stack { .. } | LayoutInput::Absolute { .. } => {
                self.placed(size, [false, false])
            }
            LayoutInput::Scroll { axis, .. } | LayoutInput::AbsoluteRows { axis, .. } => {
                Self::pack(axis, false, self.on(cross_of(axis)))
            }
            LayoutInput::Grid { align_items, .. } => Self {
                width: !matches!(
                    size.width,
                    Length::Fit | Length::MinContent | Length::MaxContent
                ),
                height: match size.height {
                    Length::Fit | Length::MinContent | Length::MaxContent => false,
                    Length::Fill { .. } => align_items == AlignItems::Stretch,
                    Length::Fixed(_) | Length::Relative { .. } => true,
                },
            },
            LayoutInput::Leaf { .. } => self,
        }
    }
}

impl Basis {
    /// The basis a child of a stack or of an absolute container on this basis
    /// lays out its own children on, `pinned` saying on each axis whether the
    /// child is pinned to both edges, which stretches a fit one.
    pub fn placed(self, size: Size, pinned: [bool; 2]) -> Self {
        let axis = |length: Length, definite: bool, pinned: bool| match length {
            Length::Fixed(_) => true,
            Length::Relative { fixed, .. } => definite || fixed != 0.0,
            Length::Fill { .. } => definite,
            Length::Fit => definite && pinned,
            Length::MinContent | Length::MaxContent => false,
        };
        Self {
            width: axis(size.width, self.width, pinned[0]),
            height: axis(size.height, self.height, pinned[1]),
        }
    }
}

/// The extent a `Relative` length `fixed + pct × basis` takes: on a definite
/// basis the sum, never negative; on an indefinite one a pure percentage falls
/// back to `fallback` (what `Fit` would take) and a mixed length counts its
/// percent as `0`, and layout reports [`LengthIssue::IndefiniteBasis`].
#[inline]
fn relative_extent(fixed: f32, pct: f32, basis: f32, definite: bool, fallback: f32) -> f32 {
    if definite {
        (fixed + pct * basis).max(0.0)
    } else if fixed == 0.0 {
        fallback
    } else {
        fixed.max(0.0)
    }
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
    if tree.hidden(root) || tree.out_of_flow(root) {
        tree.set_measured(root, Measured::default());
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
            // A content leaf (text/image/path) resolves a content-sized axis
            // against its intrinsic size; a bare layout leaf has none and reads
            // back 0 on both axes.
            match tree.content_natural(root) {
                Some(content) => Measured::sized(size, content, tree.content_min_width(root)),
                None => Measured::sized(size, Vec2::ZERO, 0.0),
            }
        }
        LayoutInput::Flex {
            axis,
            gap,
            padding,
            size,
            ..
        } => {
            // The main extents add up and the cross ones take the widest; a
            // row's narrowest width is its children's added, a column's its
            // narrowest child's.
            let mut main_sum = 0.0f32;
            let mut cross_max = 0.0f32;
            let mut min_w = 0.0f32;
            for i in 0..child_count {
                let child = scratch[start + i];
                let cm = tree.measured(child);
                let margin = margin_of(tree, child, INDEFINITE);
                main_sum += cm.on(axis) + margin.main(axis);
                cross_max = cross_max.max(cm.on(cross_of(axis)) + margin.cross(axis));
                let narrowest = cm.min_w + margin.main(Axis::Row);
                min_w = match axis {
                    Axis::Row => min_w + narrowest,
                    Axis::Column => min_w.max(narrowest),
                };
            }
            if child_count > 1 {
                main_sum += gap * (child_count as f32 - 1.0);
                if axis == Axis::Row {
                    min_w += gap * (child_count as f32 - 1.0);
                }
            }
            main_sum += padding.main(axis);
            cross_max += padding.cross(axis);
            let content = axis_pack_vec(axis, main_sum, cross_max);
            Measured::sized(size, content, min_w + padding.main(Axis::Row))
        }
        LayoutInput::Stack { padding, size, .. } => {
            let (mut w, mut h, mut min_w) = (0.0f32, 0.0f32, 0.0f32);
            for i in 0..child_count {
                let child = scratch[start + i];
                let m = tree.measured(child);
                let margin = margin_of(tree, child, INDEFINITE);
                w = w.max(m.w + margin.main(Axis::Row));
                h = h.max(m.h + margin.main(Axis::Column));
                min_w = min_w.max(m.min_w + margin.main(Axis::Row));
            }
            let content = Vec2 {
                x: w + padding.main(Axis::Row),
                y: h + padding.main(Axis::Column),
            };
            Measured::sized(size, content, min_w + padding.main(Axis::Row))
        }
        LayoutInput::Absolute { padding, size } => {
            let content = Vec2 {
                x: padding.main(Axis::Row),
                y: padding.main(Axis::Column),
            };
            Measured::sized(size, content, content.x)
        }
        LayoutInput::Scroll {
            axis, size, gutter, ..
        } => {
            // A viewport's natural size is its own request: a Fixed axis is its
            // pixel value; a content-sized axis hugs the single content child's
            // natural extent (the content can still exceed the resolved box —
            // that overflow is what scrolls, decided at layout time), with the
            // room its bar keeps across. Content that scrolls across asks no
            // width of it.
            let kids = &scratch[start..start + child_count];
            let (natural, min_w) = match kids.iter().find(|&&k| !tree.chrome(k)) {
                Some(&content) => {
                    let margin = margin_of(tree, content, INDEFINITE);
                    let m = tree.measured(content);
                    let natural = Vec2 {
                        x: m.w + margin.main(Axis::Row),
                        y: m.h + margin.main(Axis::Column),
                    };
                    let min_w = match axis {
                        Axis::Row => 0.0,
                        Axis::Column => m.min_w + margin.main(Axis::Row),
                    };
                    (natural, min_w)
                }
                None => (Vec2::ZERO, 0.0),
            };
            let (natural, min_w) = match axis {
                Axis::Column => (
                    Vec2 {
                        x: natural.x + gutter,
                        ..natural
                    },
                    min_w + gutter,
                ),
                Axis::Row => (
                    Vec2 {
                        y: natural.y + gutter,
                        ..natural
                    },
                    min_w,
                ),
            };
            Measured::sized(size, natural, min_w)
        }
        LayoutInput::AbsoluteRows { size, .. } => {
            // The canvas measures to its own declared size — the full logical
            // extent — never the sum of its sparse mounted children. Both axes
            // are Fixed in normal use; a non-Fixed axis falls back to 0 (there is
            // no content sum to hug, by design).
            Measured::sized(size, Vec2::ZERO, 0.0)
        }
        LayoutInput::Grid {
            column_count,
            column_gap,
            row_gap,
            padding,
            size,
            auto_rows,
            adaptive_columns,
            ..
        } => {
            // A content-sized axis hugs the tracks at their content: each
            // track its cells' max-content extent (a fixed one its value, a
            // percentage nothing, no box being known), the min-content width
            // its cells' min-content widths. An adaptive template's column
            // count follows the width it is given, so its columns hug only
            // their minimums.
            let children = &scratch[start..start + child_count];
            let (content, min_w) = with_grid_scratch(tree, |tree, s| {
                let cols = match adaptive_columns {
                    Some(_) => 1,
                    None => column_count.max(1),
                };
                s.placements
                    .extend(children.iter().map(|&child| tree.grid_placement(child)));
                let row_used = crate::grid::place_children(
                    cols,
                    &s.placements,
                    &mut s.occupied,
                    &mut s.regions,
                );
                match (adaptive_columns, tree.grid_column_tracks(root)) {
                    (Some(a), _) => s.col_tracks.push(TrackSizing::Fixed(a.min)),
                    (None, Some(t)) if !t.is_empty() => s.col_tracks.extend_from_slice(t),
                    (None, _) => s
                        .col_tracks
                        .extend(std::iter::repeat_n(TrackSizing::Auto, cols as usize)),
                }
                if let Some(t) = tree.grid_row_tracks(root) {
                    s.row_tracks.extend_from_slice(t);
                }
                // An empty grid makes no implicit rows.
                let row_used = if children.is_empty() { 0 } else { row_used };
                while (s.row_tracks.len() as u16) < row_used {
                    s.row_tracks.push(auto_rows);
                }
                content_contributions(&*tree, children, s, column_gap, row_gap, [0.0, 0.0]);
                let gaps = |n: usize, gap: f32| gap * n.saturating_sub(1) as f32;
                let w: f32 = s
                    .col_tracks
                    .iter()
                    .zip(&s.col_auto)
                    .map(|(&t, &c)| crate::grid::content_track(t, c))
                    .sum();
                let min_w: f32 = s
                    .col_tracks
                    .iter()
                    .zip(&s.col_min)
                    .map(|(&t, &c)| crate::grid::min_content_track(t, c))
                    .sum();
                let h: f32 = s
                    .row_tracks
                    .iter()
                    .zip(&s.row_auto)
                    .map(|(&t, &c)| crate::grid::content_track(t, c))
                    .sum();
                let col_gaps = gaps(s.col_tracks.len(), column_gap);
                let content = Vec2 {
                    x: w + col_gaps + padding.main(Axis::Row),
                    y: h + gaps(s.row_tracks.len(), row_gap) + padding.main(Axis::Column),
                };
                (content, min_w + col_gaps + padding.main(Axis::Row))
            });
            Measured::sized(size, content, min_w.min(content.x))
        }
    };

    // A node's bounds hold its natural size too, their percentages read as
    // nothing: no basis is known yet.
    let measured = match tree.rules(root) {
        Some(rules) => Measured {
            w: rules.clamp(Axis::Row, measured.w, 0.0, false),
            h: rules.clamp(Axis::Column, measured.h, 0.0, false),
            min_w: rules.clamp(Axis::Row, measured.min_w, 0.0, false),
        },
        None => measured,
    };
    tree.set_measured(root, measured);
    scratch.truncate(start);
}

/// A content box no percentage can read: both extents indefinite.
pub(crate) const INDEFINITE: Vec2 = Vec2 {
    x: f32::NAN,
    y: f32::NAN,
};

/// The margin `child` keeps in a parent content box `within` (an extent is
/// NaN where it is indefinite).
#[inline]
fn margin_of(tree: &impl LayoutTree, child: u32, within: Vec2) -> Inset {
    tree.rules(child)
        .map_or(Inset::default(), |rules| margin_in(rules, within))
}

/// The padding a container of `rules` keeps: its own `padding`, and its
/// percentage of the content box `within` it sits in, a percentage counting
/// as nothing on an indefinite extent.
#[inline(always)]
pub(crate) fn padded(padding: Inset, rules: Option<&BoxRules>, within: Vec2) -> Inset {
    let Some(rules) = rules else {
        return padding;
    };
    let pct = rules.padding_pct;
    if pct == Inset::default() {
        return padding;
    }
    let edge = |fixed: f32, f: f32, basis: f32| {
        if basis.is_nan() {
            fixed
        } else {
            (fixed + f * basis).max(0.0)
        }
    };
    Inset {
        left: edge(padding.left, pct.left, within.x),
        top: edge(padding.top, pct.top, within.y),
        right: edge(padding.right, pct.right, within.x),
        bottom: edge(padding.bottom, pct.bottom, within.y),
    }
}

/// The content box a container of `bounds` and `padding` on `basis` gives
/// its children's percentages: its extents, NaN where it is indefinite.
#[inline]
fn content_within(bounds: Rect, padding: Inset, basis: Basis) -> Vec2 {
    Vec2 {
        x: if basis.width {
            (bounds.w - padding.left - padding.right).max(0.0)
        } else {
            f32::NAN
        },
        y: if basis.height {
            (bounds.h - padding.top - padding.bottom).max(0.0)
        } else {
            f32::NAN
        },
    }
}

/// The box the root `root` takes on a surface of `surface`: the surface,
/// less its margin, held within its bounds.
pub(crate) fn root_box(tree: &impl LayoutTree, root: u32, surface: Rect) -> Rect {
    let Some(rules) = tree.rules(root) else {
        return surface;
    };
    let margin = rules.margin.resolve(surface.w, surface.h, [true, true]);
    Rect {
        x: surface.x + margin.left,
        y: surface.y + margin.top,
        w: rules.clamp(
            Axis::Row,
            surface.w - margin.main(Axis::Row),
            surface.w,
            true,
        ),
        h: rules.clamp(
            Axis::Column,
            surface.h - margin.main(Axis::Column),
            surface.h,
            true,
        ),
    }
}

/// The extent per unit of weight the fill children among `children` take
/// along `axis` so that, each held within its bounds over `basis`, they sum
/// to `free`; and that sum, which falls short of or past `free` only when
/// their bounds cannot reach it. Safeguarded Newton on the sum, which is
/// piecewise linear and non-decreasing in the share: exact once a step lands
/// in the right piece, a bisection otherwise.
fn fill_share(
    tree: &impl LayoutTree,
    children: &[u32],
    axis: Axis,
    free: f32,
    basis: f32,
    definite: bool,
) -> (f32, f32) {
    let eval = |share: f32| -> (f32, f32) {
        let (mut sum, mut slope) = (0.0f32, 0.0f32);
        for &child in children {
            let Length::Fill { weight } = tree.input(child).size().on(axis) else {
                continue;
            };
            let weight = weight.max(0.0);
            let (min, max) = tree
                .rules(child)
                .map_or((0.0, f32::INFINITY), |r| r.bounds(axis, basis, definite));
            let max = max.max(min);
            let raw = if weight > 0.0 { weight * share } else { 0.0 };
            if raw <= min {
                sum += min;
            } else if raw >= max {
                sum += max;
            } else {
                sum += raw;
                slope += weight;
            }
        }
        (sum, slope)
    };
    let (floor, _) = eval(0.0);
    if free <= floor {
        return (0.0, floor);
    }
    let (ceiling, _) = eval(f32::INFINITY);
    if free >= ceiling {
        return (f32::INFINITY, ceiling);
    }
    let (mut lo, mut hi) = (0.0f32, f32::INFINITY);
    let mut share = 1.0f32;
    for _ in 0..64 {
        let (sum, slope) = eval(share);
        let err = free - sum;
        if err.abs() <= 1e-4 * free.max(1.0) {
            return (share, sum);
        }
        if err > 0.0 {
            lo = share;
        } else {
            hi = share;
        }
        let next = if slope > 0.0 {
            share + err / slope
        } else {
            f32::NAN
        };
        share = if next > lo && next < hi {
            next
        } else if hi.is_finite() {
            (lo + hi) * 0.5
        } else {
            (share * 2.0).max(lo + 1.0)
        };
    }
    (share, eval(share).0)
}

/// The share of its width each `fit` child among `children` of a row gives
/// up so that, each no narrower than its min-content width and held within
/// its bounds over `basis`, they take `overflow` less; and how much less they
/// take, short of `overflow` only when they cannot shrink that far. The sum
/// is piecewise linear and non-decreasing in the share, solved as
/// [`fill_share`] solves its own.
fn fit_shrink(
    tree: &impl LayoutTree,
    children: &[u32],
    basis: f32,
    definite: bool,
    overflow: f32,
) -> (f32, f32) {
    let eval = |share: f32| -> (f32, f32) {
        let (mut given, mut slope) = (0.0f32, 0.0f32);
        for &child in children {
            if tree.input(child).size().width != Length::Fit {
                continue;
            }
            let m = tree.measured(child);
            let width = shrunk(m, share);
            let (held, full) = match tree.rules(child) {
                Some(rules) => (
                    rules.clamp(Axis::Row, width, basis, definite),
                    rules.clamp(Axis::Row, m.w, basis, definite),
                ),
                None => (width, m.w),
            };
            given += full - held;
            if held == width && m.w * (1.0 - share) > m.min_w {
                slope += m.w;
            }
        }
        (given, slope)
    };
    let (most, _) = eval(1.0);
    if most <= overflow {
        return (1.0, most);
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let mut share = 0.0f32;
    for _ in 0..64 {
        let (given, slope) = eval(share);
        let err = overflow - given;
        if err.abs() <= 1e-4 * overflow.max(1.0) {
            return (share, given);
        }
        if err > 0.0 {
            lo = share;
        } else {
            hi = share;
        }
        let next = if slope > 0.0 {
            share + err / slope
        } else {
            f32::NAN
        };
        share = if next > lo && next < hi {
            next
        } else {
            (lo + hi) * 0.5
        };
    }
    (share, eval(share).0)
}

/// The width a fit child measured `m` takes giving up `share` of it: never
/// narrower than its min-content width.
#[inline]
fn shrunk(m: Measured, share: f32) -> f32 {
    (m.w * (1.0 - share)).max(m.min_w)
}

/// The main-axis extent flex child `child` requests by `length` over a
/// content box `extent` along `axis`, a fill its natural one.
#[inline(always)]
fn main_request(
    tree: &impl LayoutTree,
    child: u32,
    length: Length,
    axis: Axis,
    extent: f32,
    definite: bool,
) -> f32 {
    match length {
        Length::Fixed(v) => v,
        Length::Fit | Length::Fill { .. } | Length::MinContent | Length::MaxContent => {
            tree.measured(child).on(axis)
        }
        Length::Relative { fixed, pct } => {
            relative_extent(fixed, pct, extent, definite, tree.measured(child).on(axis))
        }
    }
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
    let within = Vec2 {
        x: bounds.w,
        y: bounds.h,
    };
    let bounds = root_box(tree, root, bounds);
    layout_on(tree, root, bounds, Basis::DEFINITE, within, scratch);
}

/// [`layout`] a node whose box is a percent basis only where `basis` says,
/// in a parent content box `within` (NaN where indefinite) its percentage
/// padding reads: a subtree re-laid in place passes what its ancestry gives
/// it.
pub fn layout_on(
    tree: &mut impl LayoutTree,
    root: u32,
    bounds: Rect,
    basis: Basis,
    within: Vec2,
    scratch: &mut Vec<u32>,
) {
    // A hidden subtree lays out to a zero rect at the parent-assigned origin and
    // its children are never placed — it measured to zero, so the parent already
    // gave it a zero-extent slot; short-circuiting here also spares the whole
    // subtree's layout recursion. A node out of flow keeps the box its own
    // layout gives it.
    if tree.out_of_flow(root) {
        return;
    }
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

    let (axis, gap, padding, align, justify) = match tree.input(root) {
        LayoutInput::Flex {
            axis,
            gap,
            padding,
            align,
            justify,
            ..
        } => (axis, gap, padding, align, justify),
        LayoutInput::Scroll { axis, .. } => {
            layout_scroll(tree, root, bounds, axis, basis, scratch);
            return;
        }
        LayoutInput::AbsoluteRows { axis, .. } => {
            layout_absolute_rows(tree, root, bounds, axis, basis, scratch);
            return;
        }
        LayoutInput::Stack { padding, align, .. } => {
            let padding = padded(padding, tree.keep_within(root, within), within);
            layout_placed(tree, root, bounds, basis, padding, Some(align), scratch);
            return;
        }
        LayoutInput::Absolute { padding, .. } => {
            let padding = padded(padding, tree.keep_within(root, within), within);
            layout_placed(tree, root, bounds, basis, padding, None, scratch);
            return;
        }
        LayoutInput::Leaf { .. } => return, // Leaf: bounds are final.
        LayoutInput::Grid { .. } => {
            // Top-level grid (or a grid child of a non-grid parent): it solves
            // both of its own axes — no inherited tracks.
            layout_grid(tree, root, bounds, within, None, None, scratch);
            return;
        }
    };
    let padding = padded(padding, tree.keep_within(root, within), within);

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
    // Right to left, a row runs from its right edge and a column aligns its
    // children's `start` to its right.
    let (reverse, mirrored) = match (tree.rtl(root), axis) {
        (false, _) => (false, false),
        (true, Axis::Row) => (true, false),
        (true, Axis::Column) => (false, true),
    };
    let main_definite = basis.on(axis);
    let cross_definite = basis.on(cross);
    let inner = content_within(bounds, padding, basis);
    let gaps_total = if child_count > 1 {
        gap * (child_count as f32 - 1.0)
    } else {
        0.0
    };

    // Sum the fixed/relative/fit main sizes, each within its bounds, the
    // margins, and the total fill weight in one sweep. A relative size and a
    // bound resolve against the content box's main extent. A child without
    // rules, almost every one, takes its request as is.
    let mut fixed_main = 0.0f32;
    let mut weight_total = 0.0f32;
    let mut bounded_fill = false;
    let mut ruled = false;
    for i in 0..child_count {
        let child = scratch[start + i];
        let length = tree.input(child).size().on(axis);
        if let Some(rules) = tree.rules(child) {
            ruled = true;
            fixed_main += margin_in(rules, inner).main(axis);
            match length {
                Length::Fill { weight } => {
                    weight_total += weight.max(0.0);
                    bounded_fill |= rules.bounds_on(axis);
                }
                length => {
                    let request =
                        main_request(tree, child, length, axis, main_extent, main_definite);
                    fixed_main += rules.clamp(axis, request, main_extent, main_definite);
                }
            }
            continue;
        }
        match length {
            Length::Fill { weight } => weight_total += weight.max(0.0),
            length => {
                fixed_main += main_request(tree, child, length, axis, main_extent, main_definite);
            }
        }
    }

    // A row its children overflow takes what it lacks from its fit children,
    // each giving up the same share of its width down to its min-content one.
    let overflow = fixed_main + gaps_total - main_extent;
    let shrink = if axis == Axis::Row && overflow > 0.0 {
        let children = &scratch[start..start + child_count];
        let (shrink, given) = fit_shrink(tree, children, main_extent, main_definite, overflow);
        fixed_main -= given;
        shrink
    } else {
        0.0
    };

    // What the fills share: the leftover, never negative, split by weight —
    // or, when a fill is bounded, the share per weight that lets the bounded
    // fills together take it. Alignment keeps whatever is left, signed: an
    // oversized centered group overflows equally on both sides.
    let free = main_extent - fixed_main - gaps_total;
    let (share, filled) = if weight_total <= 0.0 {
        (0.0, 0.0)
    } else if bounded_fill {
        let children = &scratch[start..start + child_count];
        fill_share(tree, children, axis, free, main_extent, main_definite)
    } else {
        (free.max(0.0) / weight_total, free.max(0.0))
    };
    let remaining = free - filled;

    // Place children along the main axis, advancing a cursor from the near
    // edge. `Justify` shifts the whole packed group by what the fills left.
    // A reversed row places each child as a left-to-right one would, its
    // right margin leading, and reflects it across the content box.
    let main_origin = rect_start(bounds, axis) + padding.main_start(axis);
    let cross_origin = rect_start(bounds, cross) + padding.cross_start(axis);
    let justify_off = match justify {
        Justify::Start => 0.0,
        Justify::Center => remaining * 0.5,
        Justify::End => remaining,
    };
    let mut cursor = main_origin + justify_off;

    // The child ids live in `scratch[start..start + child_count]`. Recursion
    // appends past that range and truncates back to its own start, so the ids
    // stay valid across the loop — no per-container snapshot allocation. Each
    // id is `Copy`d out before the recursive `&mut scratch` borrow.
    for i in 0..child_count {
        let child = scratch[start + i];
        let input = tree.input(child);
        let length = input.size().on(axis);
        if matches!(length, Length::Relative { .. }) && !main_definite {
            tree.report_length(child, LengthIssue::IndefiniteBasis);
        }
        let request = match length {
            Length::Fill { weight } => {
                if weight > 0.0 {
                    weight * share
                } else {
                    0.0
                }
            }
            Length::Fit if shrink > 0.0 => shrunk(tree.measured(child), shrink),
            length => main_request(tree, child, length, axis, main_extent, main_definite),
        };
        let rules = if ruled {
            tree.rules(child).copied()
        } else {
            None
        };
        let (at, main_size, (cross_size, cross_off), next) = match &rules {
            None => (
                cursor,
                request,
                flex_cross(
                    tree,
                    child,
                    axis,
                    align,
                    cross_extent,
                    cross_definite,
                    mirrored,
                ),
                cursor + request + gap,
            ),
            Some(rules) => {
                let margin = margin_in(rules, inner);
                let main_size = rules.clamp(axis, request, main_extent, main_definite);
                let cross = ruled_cross(
                    tree,
                    child,
                    axis,
                    (align, mirrored),
                    cross_extent,
                    cross_definite,
                    rules,
                    margin,
                );
                let lead = if reverse {
                    margin.right
                } else {
                    margin.main_start(axis)
                };
                (
                    cursor + lead,
                    main_size,
                    cross,
                    cursor + main_size + margin.main(axis) + gap,
                )
            }
        };

        let at = if reverse {
            2.0 * main_origin + main_extent - at - main_size
        } else {
            at
        };
        let child_box = axis_rect(axis, at, cross_origin + cross_off, main_size, cross_size);
        // The child's assigned box width is main on a Row, cross on a Column.
        // Record it so a wrap-eligible text child can reflow to it later; the
        // recorder is a no-op for every non-text / non-wrapping child.
        let assigned_width = if axis == Axis::Row {
            main_size
        } else {
            cross_size
        };
        tree.request_text_reflow(child, assigned_width);
        let child_basis = basis.child(tree.input(root), input);
        layout_on(tree, child, child_box, child_basis, inner, scratch);
        cursor = next;
    }

    // Release the child-id slice back to the caller's scratch high-water mark.
    scratch.truncate(start);
}

/// The margin `rules` keep in a content box `within`.
#[inline]
fn margin_in(rules: &BoxRules, within: Vec2) -> Inset {
    rules
        .margin
        .resolve(within.x, within.y, [!within.x.is_nan(), !within.y.is_nan()])
}

/// The cross extent of flex child `child` of a container laying out along
/// `axis` with `align`, over a content box `extent` across, and its offset
/// into that box. A `fill` takes the whole extent under any `align`; a `fit`
/// takes its content, or the whole extent under `stretch`; a fixed or relative
/// request takes its own. `align` then places what does not fill, from the
/// far edge when `mirrored`.
#[inline(always)]
fn flex_cross(
    tree: &mut impl LayoutTree,
    child: u32,
    axis: Axis,
    align: Align,
    extent: f32,
    definite: bool,
    mirrored: bool,
) -> (f32, f32) {
    let size = cross_request(tree, child, axis, align, extent, extent, definite);
    (size, aligned(align, mirrored, extent, size))
}

/// [`flex_cross`] for a child with `rules` and a `margin`: what fills takes
/// the extent less the margin, a relative request still reads the whole
/// extent, the bounds hold the result, and `align` places it inside the
/// margin, from the far edge when mirrored.
#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn ruled_cross(
    tree: &mut impl LayoutTree,
    child: u32,
    axis: Axis,
    (align, mirrored): (Align, bool),
    extent: f32,
    definite: bool,
    rules: &BoxRules,
    margin: Inset,
) -> (f32, f32) {
    let room = extent - margin.cross(axis);
    let request = cross_request(tree, child, axis, align, room, extent, definite);
    let size = rules.clamp(cross_of(axis), request, extent, definite);
    (
        size,
        margin.cross_start(axis) + aligned(align, mirrored, room, size),
    )
}

/// The cross extent flex child `child` requests: `room` for a fill, or a fit
/// under `stretch`; its content within `room` for a fit, whatever the room
/// for a content size; its own for a fixed or relative request, a relative
/// one over the content box `extent`.
#[inline(always)]
fn cross_request(
    tree: &mut impl LayoutTree,
    child: u32,
    axis: Axis,
    align: Align,
    room: f32,
    extent: f32,
    definite: bool,
) -> f32 {
    match tree.input(child).size().cross(axis) {
        Length::Fill { .. } => room,
        Length::Fit if align == Align::Stretch => room,
        Length::Fit => tree.measured(child).fit(cross_of(axis), room),
        Length::MinContent | Length::MaxContent => tree.measured(child).on(cross_of(axis)),
        Length::Fixed(v) => v,
        Length::Relative { fixed, pct } => {
            if !definite {
                tree.report_length(child, LengthIssue::IndefiniteBasis);
            }
            let natural = tree.measured(child).on(cross_of(axis));
            relative_extent(fixed, pct, extent, definite, natural)
        }
    }
}

/// Where `align` places an extent `size` in `room`, read from the far edge
/// when `mirrored`.
#[inline(always)]
fn aligned(align: Align, mirrored: bool, room: f32, size: f32) -> f32 {
    let offset = align_offset(align, room, size);
    if mirrored {
        room - size - offset
    } else {
        offset
    }
}

/// Where `align` places an extent `size` in `room`.
#[inline(always)]
fn align_offset(align: Align, room: f32, size: f32) -> f32 {
    match align {
        Align::Start | Align::Stretch => 0.0,
        Align::Center => (room - size) * 0.5,
        Align::End => room - size,
    }
}

/// The box flex container `parent`, laid out in `bounds` on `basis` in a
/// content box `within`, gives its child `child` when only the child's
/// measure changed and its main-axis request does not follow it: the
/// main-axis run it was placed in, `kept`, across the cross extent its new
/// measure takes. Queues the text reflow the container's pass would. `None`
/// when `parent` is not a flex container.
fn flex_child_box(
    tree: &mut impl LayoutTree,
    parent: u32,
    child: u32,
    bounds: Rect,
    basis: Basis,
    within: Vec2,
    kept: Rect,
) -> Option<Rect> {
    let LayoutInput::Flex {
        axis,
        padding,
        align,
        ..
    } = tree.input(parent)
    else {
        return None;
    };
    let padding = padded(padding, tree.rules(parent), within);
    let cross = cross_of(axis);
    let extent = rect_len(bounds, cross) - padding.cross(axis);
    let mirrored = axis == Axis::Column && tree.rtl(parent);
    let (cross_size, cross_off) = match tree.rules(child).copied() {
        None => flex_cross(tree, child, axis, align, extent, basis.on(cross), mirrored),
        Some(rules) => {
            let margin = margin_in(&rules, content_within(bounds, padding, basis));
            ruled_cross(
                tree,
                child,
                axis,
                (align, mirrored),
                extent,
                basis.on(cross),
                &rules,
                margin,
            )
        }
    };
    let main_size = rect_len(kept, axis);
    tree.request_text_reflow(
        child,
        if axis == Axis::Row {
            main_size
        } else {
            cross_size
        },
    );
    Some(axis_rect(
        axis,
        rect_start(kept, axis),
        rect_start(bounds, cross) + padding.cross_start(axis) + cross_off,
        main_size,
        cross_size,
    ))
}

/// Lay out the children of a stack, whose children sit at `align` unless
/// they align themselves, or of an absolute container (`align` `None`),
/// whose children sit at their insets: each in the content box of `bounds`
/// less `padding`, alone, no sibling entering into its box.
fn layout_placed(
    tree: &mut impl LayoutTree,
    root: u32,
    bounds: Rect,
    basis: Basis,
    padding: Inset,
    align: Option<Alignment2D>,
    scratch: &mut Vec<u32>,
) {
    let start = scratch.len();
    tree.children(root, scratch);
    let content = inner_rect(bounds, padding);
    let inner = content_within(bounds, padding, basis);
    let rtl = tree.rtl(root);
    // Recursion appends past this run and truncates back to it.
    for i in start..scratch.len() {
        let child = scratch[i];
        let (child_box, pinned) = match align {
            Some(align) => (
                stack_box(tree, child, content, rtl, basis, inner, align),
                [false; 2],
            ),
            None => absolute_box(tree, child, content, rtl, basis, inner),
        };
        let child_basis = basis.placed(tree.input(child).size(), pinned);
        layout_on(tree, child, child_box, child_basis, inner, scratch);
    }
    scratch.truncate(start);
}

/// `bounds` less `padding`, never negative.
#[inline]
fn inner_rect(bounds: Rect, padding: Inset) -> Rect {
    Rect {
        x: bounds.x + padding.left,
        y: bounds.y + padding.top,
        w: (bounds.w - padding.left - padding.right).max(0.0),
        h: (bounds.h - padding.top - padding.bottom).max(0.0),
    }
}

/// The box a stack gives its child `child` in its content box `content`,
/// on `basis`, its extents `inner` (NaN where indefinite): its size, at
/// its own alignment or the stack's `align`, inside its margin. Queues the
/// text reflow its width allows.
fn stack_box(
    tree: &mut impl LayoutTree,
    child: u32,
    content: Rect,
    rtl: bool,
    basis: Basis,
    inner: Vec2,
    align: Alignment2D,
) -> Rect {
    let align = match tree.placement(child) {
        Some(Placement::Stack(placed)) => placed.align.unwrap_or(align),
        _ => align,
    };
    let (fx, fy) = align.fractions();
    // Right to left, `start` is the right edge.
    let fx = if rtl { 1.0 - fx } else { fx };
    let rules = tree.rules(child).copied();
    let margin = rules
        .as_ref()
        .map_or(Inset::default(), |r| margin_in(r, inner));
    let size = tree.input(child).size();
    let room = |extent: f32, axis: Axis| extent - margin.main(axis);
    let w = placed_extent(
        tree,
        child,
        Axis::Row,
        (room(content.w, Axis::Row), content.w, basis.width),
        false,
        size.width,
        rules.as_ref(),
    );
    let h = placed_extent(
        tree,
        child,
        Axis::Column,
        (room(content.h, Axis::Column), content.h, basis.height),
        false,
        size.height,
        rules.as_ref(),
    );
    tree.request_text_reflow(child, w);
    Rect {
        x: content.x + margin.left + (room(content.w, Axis::Row) - w) * fx,
        y: content.y + margin.top + (room(content.h, Axis::Column) - h) * fy,
        w,
        h,
    }
}

/// The box an absolute container gives its child `child` in its content box
/// `content`, on `basis`, its extents `inner` (NaN where indefinite): pinned
/// by its insets to the edges they name — at the start and top where it
/// names neither — inside its margin; and on each axis whether it is
/// pinned to both edges, which stretches a fit child between them. Queues
/// the text reflow its width allows.
fn absolute_box(
    tree: &mut impl LayoutTree,
    child: u32,
    content: Rect,
    rtl: bool,
    basis: Basis,
    inner: Vec2,
) -> (Rect, [bool; 2]) {
    let insets = match tree.placement(child) {
        Some(Placement::Absolute(insets)) => *insets,
        _ => AbsoluteInsets::NONE,
    };
    let rules = tree.rules(child).copied();
    let margin = rules
        .as_ref()
        .map_or(Inset::default(), |r| margin_in(r, inner));
    let size = tree.input(child).size();
    // Right to left, `start` is the right edge: the near (left) one is `end`,
    // and a child pinned to neither sits at the right.
    let mut place = |axis: Axis, origin: f32, extent: f32, definite: bool| {
        let flip = rtl && axis == Axis::Row;
        let (near, far) = match insets.on(axis) {
            (start, end) if flip => (end, start),
            edges => edges,
        };
        let mut edge = |span: Option<Span>| {
            let span = span?;
            if span.pct != 0.0 && !definite {
                tree.report_length(child, LengthIssue::IndefiniteBasis);
            }
            Some(span.resolve(extent, definite, 0.0))
        };
        let (near, far) = (edge(near), edge(far));
        let pinned = near.is_some() && far.is_some();
        let room = extent - near.unwrap_or(0.0) - far.unwrap_or(0.0) - margin.main(axis);
        let length = size.on(axis);
        let len = placed_extent(
            tree,
            child,
            axis,
            (room, extent, definite),
            pinned,
            length,
            rules.as_ref(),
        );
        let lead = margin.main_start(axis);
        let at = match (near, far) {
            (Some(near), _) => near + lead,
            (None, Some(far)) => extent - far - len - (margin.main(axis) - lead),
            (None, None) if flip => extent - len - (margin.main(axis) - lead),
            (None, None) => lead,
        };
        (origin + at, len, pinned)
    };
    let (x, w, across) = place(Axis::Row, content.x, content.w, basis.width);
    let (y, h, down) = place(Axis::Column, content.y, content.h, basis.height);
    tree.request_text_reflow(child, w);
    (Rect { x, y, w, h }, [across, down])
}

/// The extent a stack or absolute child requests along `axis` by `length`,
/// given its `room` (its parent's content extent less its margin and
/// insets), that `extent` and whether it is definite: the room for a fill
/// or a `stretch`ed fit, its content within the room for a fit, whatever the
/// room for a content size, its own for a fixed or relative request, held
/// within its bounds.
#[inline]
fn placed_extent(
    tree: &mut impl LayoutTree,
    child: u32,
    axis: Axis,
    (room, extent, definite): (f32, f32, bool),
    stretch: bool,
    length: Length,
    rules: Option<&BoxRules>,
) -> f32 {
    let request = match length {
        Length::Fill { .. } => room.max(0.0),
        Length::Fit if stretch => room.max(0.0),
        Length::Fit => tree.measured(child).fit(axis, room),
        Length::MinContent | Length::MaxContent => tree.measured(child).on(axis),
        Length::Fixed(v) => v,
        Length::Relative { fixed, pct } => {
            if !definite {
                tree.report_length(child, LengthIssue::IndefiniteBasis);
            }
            let natural = tree.measured(child).on(axis);
            relative_extent(fixed, pct, extent, definite, natural)
        }
    };
    rules.map_or(request, |rules| {
        rules.clamp(axis, request, extent, definite)
    })
}

/// The box container `parent`, laid out in `bounds` on `basis` in a content
/// box `within`, gives its child `child` when only the child's measure
/// changed and no sibling moves with it: a flex container's (see
/// [`flex_child_box`]), a stack's or an absolute container's. `None` for
/// any other container.
#[inline(never)]
pub(crate) fn placed_child_box(
    tree: &mut impl LayoutTree,
    parent: u32,
    child: u32,
    bounds: Rect,
    basis: Basis,
    within: Vec2,
    kept: Rect,
) -> Option<Rect> {
    let (padding, align) = match tree.input(parent) {
        LayoutInput::Flex { .. } => {
            return flex_child_box(tree, parent, child, bounds, basis, within, kept);
        }
        LayoutInput::Stack { padding, align, .. } => (padding, Some(align)),
        LayoutInput::Absolute { padding, .. } => (padding, None),
        _ => return None,
    };
    let padding = padded(padding, tree.rules(parent), within);
    let content = inner_rect(bounds, padding);
    let inner = content_within(bounds, padding, basis);
    let rtl = tree.rtl(parent);
    Some(match align {
        Some(align) => stack_box(tree, child, content, rtl, basis, inner, align),
        None => absolute_box(tree, child, content, rtl, basis, inner).0,
    })
}

/// Whether an absolute container's child `child` is pinned to both edges of
/// each axis.
pub(crate) fn pinned(tree: &impl LayoutTree, child: u32) -> [bool; 2] {
    match tree.placement(child) {
        Some(Placement::Absolute(insets)) => [
            insets.start.is_some() && insets.end.is_some(),
            insets.top.is_some() && insets.bottom.is_some(),
        ],
        _ => [false; 2],
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
    basis: Basis,
    scratch: &mut Vec<u32>,
) {
    let start = scratch.len();
    tree.children(root, scratch);
    let kids = &scratch[start..];
    // A viewport hosts a single content subtree; its chrome (its scroll
    // bars) lays out along its edge and does not scroll.
    let chrome = kids.iter().any(|&k| tree.chrome(k));
    let content = kids.iter().copied().find(|&k| !tree.chrome(k));
    let cross = cross_of(axis);
    let bar_first = axis == Axis::Column && tree.rtl(root);
    let Some(content) = content else {
        scratch.truncate(start);
        tree.set_content(root, Vec2::ZERO);
        if chrome {
            layout_scroll_chrome(tree, root, bounds, axis, bar_first, scratch);
        }
        return;
    };

    // Content takes its natural main extent (unclamped → overflow scrolls) and
    // fills the viewport across.
    let gutter = match tree.input(root) {
        LayoutInput::Scroll { gutter, .. } => gutter.clamp(0.0, rect_len(bounds, cross)),
        _ => 0.0,
    };
    // The bar sits at the end across, at the start in a column read right to
    // left.
    let viewport = bounds;
    let bounds = match (axis, bar_first) {
        (Axis::Column, false) => Rect {
            w: bounds.w - gutter,
            ..bounds
        },
        (Axis::Column, true) => Rect {
            x: bounds.x + gutter,
            w: bounds.w - gutter,
            ..bounds
        },
        (Axis::Row, _) => Rect {
            h: bounds.h - gutter,
            ..bounds
        },
    };
    let main_size = tree.measured(content).on(axis);
    let inner = content_within(
        bounds,
        Inset::default(),
        Basis::pack(axis, false, basis.on(cross)),
    );
    let (margin, cross_size) = match tree.rules(content) {
        Some(rules) => {
            let margin = margin_in(rules, inner);
            let room = rect_len(bounds, cross) - margin.cross(axis);
            let extent = rect_len(bounds, cross);
            (margin, rules.clamp(cross, room, extent, basis.on(cross)))
        }
        None => (Inset::default(), rect_len(bounds, cross)),
    };
    scratch.truncate(start);

    // A row read right to left starts at its right edge: the content's right
    // edge meets the viewport's, and it scrolls toward its left.
    let main_start = if axis == Axis::Row && tree.rtl(root) {
        bounds.x + bounds.w - margin.right - main_size
    } else {
        rect_start(bounds, axis) + margin.main_start(axis)
    };
    let content_box = axis_rect(
        axis,
        main_start,
        rect_start(bounds, cross) + margin.cross_start(axis),
        main_size,
        cross_size,
    );
    tree.set_content(
        root,
        axis_pack_vec(
            axis,
            main_size + margin.main(axis),
            cross_size + margin.cross(axis),
        ),
    );
    // The scrolling axis is unbounded: a fill there takes the content's
    // natural extent, as a fit does.
    let input = tree.input(content);
    if matches!(input.size().on(axis), Length::Fill { .. }) {
        tree.report_length(content, LengthIssue::IndefiniteBasis);
    }
    let content_basis = basis.child(tree.input(root), input);
    layout_on(tree, content, content_box, content_basis, inner, scratch);
    if chrome {
        layout_scroll_chrome(tree, root, viewport, axis, bar_first, scratch);
    }
}

/// Lay out a viewport's chrome: each a track along the whole viewport, as
/// thick across as it asks, at the end across (at the start when
/// `bar_first`).
fn layout_scroll_chrome(
    tree: &mut impl LayoutTree,
    root: u32,
    viewport: Rect,
    axis: Axis,
    bar_first: bool,
    scratch: &mut Vec<u32>,
) {
    let cross = cross_of(axis);
    let start = scratch.len();
    tree.children(root, scratch);
    let count = scratch.len() - start;
    for k in 0..count {
        let track = scratch[start + k];
        if !tree.chrome(track) {
            continue;
        }
        let thick = match tree.input(track).size().on(cross) {
            Length::Fixed(t) => t.clamp(0.0, rect_len(viewport, cross)),
            _ => 0.0,
        };
        let across = if bar_first {
            rect_start(viewport, cross)
        } else {
            rect_start(viewport, cross) + rect_len(viewport, cross) - thick
        };
        let rect = axis_rect(
            axis,
            rect_start(viewport, axis),
            across,
            rect_len(viewport, axis),
            thick,
        );
        let within = Vec2 {
            x: rect.w,
            y: rect.h,
        };
        layout_on(tree, track, rect, Basis::DEFINITE, within, scratch);
    }
    scratch.truncate(start);
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
    basis: Basis,
    scratch: &mut Vec<u32>,
) {
    let start = scratch.len();
    tree.children(root, scratch);
    let child_count = scratch.len() - start;
    if child_count == 0 {
        scratch.truncate(start);
        return;
    }

    // The child ids sit in `scratch[start..start + child_count]`. A recursive
    // `layout` leaves `scratch` at the length it entered with (every arm pushes
    // its own children and truncates them back), so that window stays intact
    // across the loop — we index it directly and never snapshot, keeping the
    // pass allocation-free.
    for k in 0..child_count {
        let child = scratch[start + k];
        let Some(child_box) = row_box(tree, root, child, bounds, axis) else {
            continue;
        };
        let child_basis = basis.child(tree.input(root), tree.input(child));
        let inner = content_within(
            bounds,
            Inset::default(),
            Basis::pack(axis, false, basis.on(cross_of(axis))),
        );
        layout_on(tree, child, child_box, child_basis, inner, scratch);
    }
    scratch.truncate(start);
}

/// The box an [`LayoutInput::AbsoluteRows`] canvas `index` at `canvas`
/// along `axis` gives its positioned child `child`: at its row offset from
/// the canvas's start (its right edge for a row read right to left), its
/// measured main extent, the canvas's whole cross extent. `None` for a
/// child that is not a positioned row. No sibling enters into it, so a row
/// that re-measures is re-placed alone.
pub(crate) fn row_box(
    tree: &impl LayoutTree,
    index: u32,
    child: u32,
    canvas: Rect,
    axis: Axis,
) -> Option<Rect> {
    let offset = tree.row_offset(child)?;
    let cross = cross_of(axis);
    let across = rect_len(canvas, cross);
    let cross_size = match tree.rules(child) {
        Some(rules) => rules.clamp(cross, across, across, true),
        None => across,
    };
    let main = tree.measured(child).on(axis);
    let main_start = if axis == Axis::Row && tree.rtl(index) {
        canvas.x + canvas.w - offset - main
    } else {
        rect_start(canvas, axis) + offset
    };
    Some(axis_rect(
        axis,
        main_start,
        rect_start(canvas, cross),
        main,
        cross_size,
    ))
}

/// The working buffers of one grid layout. `layout_grid` needs about a dozen
/// temporary `Vec`s (placements, the occupied bitset, cell regions, the two
/// track templates, per-track auto maxes, resolved sizes, prefix offsets, and
/// row baselines). Allocating them fresh on every call would heap-allocate on
/// the layout hot path for every grid node every frame; instead each call takes
/// a set from the tree ([`LayoutTree::take_grid_scratch`]), clears it (clear,
/// not free) and gives it back on exit, so a warmed-up steady-state grid
/// relayout over a tree that pools them does no per-frame allocation.
///
/// A pool rather than a single buffer because `layout_grid` is *reentrant*: a
/// subgrid child recurses into `layout_grid` from inside the parent's
/// per-child loop while the parent still holds live slices of its own
/// `col_sizes`/`row_sizes`. The recursive call takes a *distinct* set, so
/// nested grids never clobber an ancestor's buffers, and the pool grows only to
/// the deepest grid nesting seen (one set for the common non-nested case).
#[derive(Default)]
pub struct GridScratch {
    placements: Vec<GridPlacement>,
    occupied: Vec<u64>,
    regions: Vec<crate::grid::CellRegion>,
    col_tracks: Vec<TrackSizing>,
    row_tracks: Vec<TrackSizing>,
    col_auto: Vec<f32>,
    /// Each column's min-content width: the narrowest its cells lay out in.
    col_min: Vec<f32>,
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
        self.col_min.clear();
        self.row_auto.clear();
        self.col_sizes.clear();
        self.row_sizes.clear();
        self.col_offsets.clear();
        self.row_offsets.clear();
        self.row_baselines.clear();
    }
}

/// Gathers what the cells `children` (placed in `s.regions`, over the
/// templates `s.col_tracks` and `s.row_tracks`) need of each track into
/// `s.col_auto`, `s.col_min` and `s.row_auto`: the widest max-content width,
/// the widest min-content width and the tallest height of the cells on each
/// track alone, then what a spanning cell still needs beyond the tracks it
/// covers, spread across their content-sized ones (see
/// `distribute_spanning_auto`). `extent` is the content box the percentages
/// of the tracks read (zero while measuring). An axis with no template — a
/// subgrid's inherited one — gathers nothing.
fn content_contributions(
    tree: &impl LayoutTree,
    children: &[u32],
    s: &mut GridScratch,
    column_gap: f32,
    row_gap: f32,
    extent: [f32; 2],
) {
    s.col_auto.resize(s.col_tracks.len(), 0.0);
    s.col_min.resize(s.col_tracks.len(), 0.0);
    s.row_auto.resize(s.row_tracks.len(), 0.0);
    for (&child, r) in children.iter().zip(&s.regions) {
        let cm = tree.measured(child);
        if r.col_span == 1 {
            let c = r.col as usize;
            if c < s.col_auto.len() {
                s.col_auto[c] = s.col_auto[c].max(cm.w);
                s.col_min[c] = s.col_min[c].max(cm.min_w);
            }
        }
        if r.row_span == 1 {
            let rr = r.row as usize;
            if rr < s.row_auto.len() {
                s.row_auto[rr] = s.row_auto[rr].max(cm.h);
            }
        }
    }
    let spans = |pick: fn(&crate::grid::CellRegion, Measured) -> (u16, u16, f32)| {
        children
            .iter()
            .zip(&s.regions)
            .map(move |(&child, r)| pick(r, tree.measured(child)))
    };
    if !s.col_tracks.is_empty() {
        crate::grid::distribute_spanning_auto(
            &s.col_tracks,
            column_gap,
            extent[0],
            spans(|r, m| (r.col, r.col_span, m.w)),
            &mut s.col_auto,
        );
        crate::grid::distribute_spanning_auto(
            &s.col_tracks,
            column_gap,
            extent[0],
            spans(|r, m| (r.col, r.col_span, m.min_w)),
            &mut s.col_min,
        );
    }
    if !s.row_tracks.is_empty() {
        crate::grid::distribute_spanning_auto(
            &s.row_tracks,
            row_gap,
            extent[1],
            spans(|r, m| (r.row, r.row_span, m.h)),
            &mut s.row_auto,
        );
    }
}

/// Run `f` over a cleared `GridScratch` taken from `tree`, giving it back
/// afterward. Reentrant: a nested call takes a distinct set, so a subgrid
/// recursion never aliases its ancestor's scratch.
fn with_grid_scratch<T: LayoutTree, R>(
    tree: &mut T,
    f: impl FnOnce(&mut T, &mut GridScratch) -> R,
) -> R {
    let mut s = tree.take_grid_scratch();
    s.clear();
    let r = f(tree, &mut s);
    tree.put_grid_scratch(s);
    r
}

/// Lay out a grid: place children into cells (auto-flow + explicit), solve the
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
    within: Vec2,
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
    let padding = padded(padding, tree.keep_within(root, within), within);
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

    // A gap's percentage reads the content box: the width always, the height
    // where the grid's own is definite (a content-sized height counts it as
    // nothing, as measuring did).
    let [col_pct, row_pct] = tree.grid_gap_pct(root);
    let column_gap = match inherited_cols {
        None => column_gap + col_pct * content_w,
        Some(_) => column_gap,
    };
    let definite_height = !matches!(
        tree.input(root).size().height,
        Length::Fit | Length::MinContent | Length::MaxContent
    );
    let row_gap = match inherited_rows {
        None if definite_height => row_gap + row_pct * content_h,
        _ => row_gap,
    };

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

    // Take a `GridScratch` from the tree for the ~dozen working buffers,
    // instead of allocating them fresh per call. `with_grid_scratch` clears
    // (not frees) and gives it back on exit; a subgrid recursion below takes a
    // distinct set, so it never clobbers `s`.
    with_grid_scratch(tree, |tree, s| {
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
        let children = &scratch[start..start + child_count];
        content_contributions(
            tree,
            children,
            s,
            column_gap,
            row_gap,
            [content_w, content_h],
        );

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
                Some(&s.col_min),
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
                None,
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
        // offset(start) .. offset(start+span) minus the trailing gap. Right to
        // left, columns run from the content box's right edge and a child hugs
        // its cell's right edge.
        let rtl = tree.rtl(root);
        for i in 0..child_count {
            let child = scratch[start + i];
            let r = s.regions[i];
            let cx0 = s.col_offsets[r.col as usize];
            let cx1 = span_end(&s.col_offsets, &s.col_sizes, r.col, r.col_span, column_gap);
            let ry0 = s.row_offsets[r.row as usize];
            let ry1 = span_end(&s.row_offsets, &s.row_sizes, r.row, r.row_span, row_gap);
            let area = Rect {
                x: if rtl {
                    origin_x + content_w - cx1
                } else {
                    origin_x + cx0
                },
                y: origin_y + ry0,
                w: (cx1 - cx0).max(0.0),
                h: (ry1 - ry0).max(0.0),
            };
            // The child's area is its containing box: its margin and bounds read
            // it, and the margin insets the cell it is sized and aligned in.
            let rules = tree.rules(child);
            let cell = match rules {
                None => area,
                Some(rules) => {
                    let margin = rules.margin.resolve(area.w, area.h, [true, true]);
                    Rect {
                        x: area.x + margin.left,
                        y: area.y + margin.top,
                        w: area.w - margin.main(Axis::Row),
                        h: area.h - margin.main(Axis::Column),
                    }
                }
            };
            // Horizontal (inline axis): the child fills its cell when it requests
            // Fill; a fit child takes its content within the cell, a content
            // size or a Fixed child its own, at the cell's start edge, and a
            // Relative child resolves against the cell width.
            let size = tree.input(child).size();
            let cw = match size.width {
                Length::Fit => tree.measured(child).fit(Axis::Row, cell.w),
                Length::MinContent | Length::MaxContent => tree.measured(child).w,
                Length::Fill { .. } => cell.w,
                len => len.resolve(area.w).unwrap_or(0.0),
            };
            // Vertical (block axis): a `Fill` child stretches to the cell unless the
            // grid overrides with a non-Stretch `align_items`; a Fixed/Fit child
            // always hugs its own size, a Relative child resolves against the cell
            // height, and either is then positioned by `align_items`.
            let stretch =
                matches!(size.height, Length::Fill { .. }) && align_items == AlignItems::Stretch;
            let ch = if stretch {
                cell.h
            } else {
                size.height
                    .resolve(area.h)
                    .unwrap_or_else(|| tree.measured(child).on(Axis::Column))
            };
            let (cw, ch) = match rules {
                Some(rules) => (
                    rules.clamp(Axis::Row, cw, area.w, true),
                    rules.clamp(Axis::Column, ch, area.h, true),
                ),
                None => (cw, ch),
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
                x: if rtl { cell.x + cell.w - cw } else { cell.x },
                y: cell.y + dy,
                w: cw,
                h: ch,
            };
            let area_within = Vec2 {
                x: area.w,
                y: area.h,
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
                layout_grid(
                    tree,
                    child,
                    child_box,
                    area_within,
                    col_slice,
                    row_slice,
                    scratch,
                );
            } else {
                let child_basis = Basis::DEFINITE.child(tree.input(root), tree.input(child));
                layout_on(tree, child, child_box, child_basis, area_within, scratch);
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
                ink: None,
                glyphs: Vec::new(),
                atlas: TextureId::new(0),
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                natural: Vec2 { x: 20.0, y: height },
                baseline,
                shaped_at_width: None,
                width_use: crate::WidthUse::Ignored,
                min_content: 20.0,
                max_content: 20.0,
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
            Measured::default(),
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
    fn justify_and_align_center_a_fit_child_on_both_axes() {
        use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
        use crate::style::BoxStyle;

        // A window-filling container that centers its single Fit-sized child on
        // both axes: `justify: Center` on the main (Row → horizontal), `align:
        // Center` on the cross (vertical). This is the Hello World construct;
        // the child must land exactly centered, and — the regression the user
        // hit — a *larger* box must keep it centered, not drift it to a corner.
        fn centered_leaf_bounds(w: f32, h: f32, child_w: f32, child_h: f32) -> Rect {
            let mut store = NodeStore::new();
            let mut leaf = None;
            let root = {
                let mut cx = BuildCx::new(&mut store);
                cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        align: Align::Center,
                        justify: Justify::Center,
                        size: Size::fill(),
                        ..Default::default()
                    },
                    |cx| {
                        let l = cx.leaf(LeafStyle {
                            size: Size::fixed(child_w, child_h),
                            style: BoxStyle::NONE,
                        });
                        leaf = Some(l.id());
                    },
                );
                cx.root().unwrap()
            };
            let leaf = leaf.unwrap();
            let mut scratch = Vec::new();
            measure(&mut store, root.index(), &mut scratch);
            layout(&mut store, root.index(), surface_local(w, h), &mut scratch);
            store.bounds(leaf)
        }

        // 600x400 box: child centered at ((600-200)/2, (400-40)/2) = (200, 180).
        let b = centered_leaf_bounds(600.0, 400.0, 200.0, 40.0);
        assert_eq!(b.x, 200.0, "centered on the main (horizontal) axis");
        assert_eq!(b.y, 180.0, "centered on the cross (vertical) axis");
        assert_eq!((b.w, b.h), (200.0, 40.0), "child keeps its Fit size");

        // Growing the box keeps the child centered (it does not drift to a
        // corner): a 1000x800 box centers at ((1000-200)/2, (800-40)/2).
        let big = centered_leaf_bounds(1000.0, 800.0, 200.0, 40.0);
        assert_eq!(big.x, 400.0, "still centered horizontally when enlarged");
        assert_eq!(big.y, 380.0, "still centered vertically when enlarged");

        // Overflow keeps the same center line instead of snapping to the near
        // edge: a 700px child in a 600px box overflows 50px on each side.
        let overflow = centered_leaf_bounds(600.0, 400.0, 700.0, 40.0);
        assert_eq!(
            overflow.x, -50.0,
            "an oversized child remains centered on the main axis"
        );
        assert_eq!(overflow.y, 180.0);
    }

    #[test]
    fn a_cross_axis_fill_fills_under_any_align_and_stretch_grows_only_a_fit() {
        use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
        use crate::style::BoxStyle;

        // A 200-wide column, its children 40 tall: what each takes across.
        fn across(align: Align, width: Length) -> (f32, f32) {
            let mut store = NodeStore::new();
            let mut leaf = None;
            let root = {
                let mut cx = BuildCx::new(&mut store);
                cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        align,
                        size: Size::fixed(200.0, 100.0),
                        ..Default::default()
                    },
                    |cx| {
                        leaf = Some(
                            cx.leaf(LeafStyle {
                                size: Size {
                                    width,
                                    height: Length::Fixed(40.0),
                                },
                                style: BoxStyle::NONE,
                            })
                            .id(),
                        );
                    },
                );
                cx.root().unwrap()
            };
            let mut scratch = Vec::new();
            measure(&mut store, root.index(), &mut scratch);
            layout(
                &mut store,
                root.index(),
                surface_local(200.0, 100.0),
                &mut scratch,
            );
            let b = store.bounds(leaf.unwrap());
            (b.x, b.w)
        }

        let fill = Length::Fill { weight: 1.0 };
        for align in [Align::Start, Align::Center, Align::End, Align::Stretch] {
            assert_eq!(across(align, fill), (0.0, 200.0), "{align:?}");
        }
        // A bare fit leaf has no content: zero wide, placed by `align`.
        assert_eq!(across(Align::Center, Length::Fit), (100.0, 0.0));
        assert_eq!(across(Align::Stretch, Length::Fit), (0.0, 200.0));
        // A fixed request keeps its own extent and centers by it.
        assert_eq!(across(Align::Center, Length::Fixed(50.0)), (75.0, 50.0));
        assert_eq!(across(Align::End, Length::Fixed(50.0)), (150.0, 50.0));
        assert_eq!(across(Align::Stretch, Length::Fixed(50.0)), (0.0, 50.0));
        // So does a relative one, by what it resolves to.
        let half = Length::Relative {
            fixed: 0.0,
            pct: 0.5,
        };
        assert_eq!(across(Align::Center, half), (50.0, 100.0));
    }

    /// A `width`×`height` row (or column) of leaves, each with its size and
    /// rules, laid out on a surface of the same size: the leaves' boxes.
    fn ruled(axis: Axis, width: f32, height: f32, leaves: &[(Size, BoxRules)]) -> Vec<Rect> {
        use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
        use crate::style::BoxStyle;

        let mut store = NodeStore::new();
        let mut ids = Vec::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis,
                    size: Size::fixed(width, height),
                    ..Default::default()
                },
                |cx| {
                    for &(size, _) in leaves {
                        ids.push(
                            cx.leaf(LeafStyle {
                                size,
                                style: BoxStyle::NONE,
                            })
                            .id(),
                        );
                    }
                },
            );
            cx.root().unwrap()
        };
        for (&id, &(_, rules)) in ids.iter().zip(leaves) {
            store.set_box_rules(id, rules);
        }
        store.layout(root, surface_local(width, height), &mut Vec::new());
        ids.iter().map(|&id| store.bounds(id)).collect()
    }

    fn rules() -> BoxRules {
        BoxRules::NONE
    }

    #[test]
    fn bounds_hold_a_fixed_a_fit_and_a_cross_extent_and_the_minimum_wins() {
        let fill = Length::Fill { weight: 1.0 };
        let boxes = ruled(
            Axis::Row,
            300.0,
            100.0,
            &[
                (
                    Size::fixed(80.0, 40.0),
                    BoxRules {
                        max_width: Span::dp(50.0),
                        min_height: Span::dp(60.0),
                        ..rules()
                    },
                ),
                (
                    Size {
                        width: Length::Fit,
                        height: fill,
                    },
                    BoxRules {
                        min_width: Span::dp(30.0),
                        max_height: Span::pct(50.0),
                        ..rules()
                    },
                ),
                (
                    Size::fixed(10.0, 10.0),
                    BoxRules {
                        min_width: Span::dp(40.0),
                        max_width: Span::dp(20.0),
                        ..rules()
                    },
                ),
            ],
        );
        assert_eq!(boxes[0], Rect::new(0.0, 0.0, 50.0, 60.0));
        assert_eq!(boxes[1], Rect::new(50.0, 0.0, 30.0, 50.0));
        assert_eq!(boxes[2], Rect::new(80.0, 0.0, 40.0, 10.0));
    }

    #[test]
    fn bounded_fills_share_what_their_bounds_leave() {
        let fill = Size {
            width: Length::Fill { weight: 1.0 },
            height: Length::Fixed(10.0),
        };
        let widths = |bounded: BoxRules| -> Vec<f32> {
            ruled(
                Axis::Row,
                300.0,
                10.0,
                &[(fill, bounded), (fill, rules()), (fill, rules())],
            )
            .iter()
            .map(|b| b.w)
            .collect()
        };
        let capped = BoxRules {
            max_width: Span::dp(50.0),
            ..rules()
        };
        assert_eq!(widths(capped), vec![50.0, 125.0, 125.0]);
        let floored = BoxRules {
            min_width: Span::dp(200.0),
            ..rules()
        };
        assert_eq!(widths(floored), vec![200.0, 50.0, 50.0]);
        // Every fill capped: what they leave is the container's to justify.
        let all = ruled(Axis::Row, 300.0, 10.0, &[(fill, capped), (fill, capped)]);
        assert_eq!((all[0].x, all[1].x, all[1].w), (0.0, 50.0, 50.0));
    }

    #[test]
    fn a_margin_keeps_room_on_both_axes_and_reads_the_parent_content_box() {
        let boxes = ruled(
            Axis::Row,
            200.0,
            100.0,
            &[
                (
                    Size {
                        width: Length::Fixed(40.0),
                        height: Length::Fill { weight: 1.0 },
                    },
                    BoxRules {
                        margin: SpanInset {
                            left: Span::dp(10.0),
                            top: Span::pct(10.0),
                            right: Span::pct(5.0),
                            bottom: Span::dp(20.0),
                        },
                        ..rules()
                    },
                ),
                (Size::fixed(30.0, 30.0), rules()),
                (
                    Size::fixed(30.0, 30.0),
                    BoxRules {
                        margin: SpanInset {
                            left: Span::dp(-5.0),
                            ..SpanInset::ZERO
                        },
                        ..rules()
                    },
                ),
            ],
        );
        // 10 left, 10% of 100 high on top, 5% of 200 wide on the right, 20 below.
        assert_eq!(boxes[0], Rect::new(10.0, 10.0, 40.0, 70.0));
        assert_eq!(boxes[1].x, 60.0);
        assert_eq!(boxes[2].x, 85.0);
    }

    #[test]
    fn right_to_left_mirrors_rows_start_alignments_insets_and_grid_columns() {
        use crate::adaptive::LayoutDirection;
        use crate::component::{
            AbsoluteStyle, BuildCx, FlexStyle, LeafStyle, NodeStore, StackStyle,
        };
        use crate::grid::GridStyle;
        use crate::style::BoxStyle;

        let mut store = NodeStore::new();
        let mut leaves = Vec::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let mut leaf = |cx: &mut BuildCx<'_>, w: f32| {
                let size = Size::fixed(w, 10.0);
                leaves.push(
                    cx.leaf(LeafStyle {
                        size,
                        style: BoxStyle::NONE,
                    })
                    .id(),
                );
            };
            let size = Size::fixed(200.0, 30.0);
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    size: Size::fixed(200.0, 300.0),
                    ..Default::default()
                },
                |cx| {
                    let row = FlexStyle {
                        axis: Axis::Row,
                        gap: 10.0,
                        size,
                        ..Default::default()
                    };
                    cx.flex(row, |cx| {
                        leaf(cx, 30.0);
                        leaf(cx, 40.0);
                    });
                    let column = FlexStyle {
                        axis: Axis::Column,
                        size,
                        ..Default::default()
                    };
                    cx.flex(column, |cx| leaf(cx, 50.0));
                    cx.stack(
                        StackStyle {
                            size,
                            ..Default::default()
                        },
                        |cx| {
                            leaf(cx, 20.0);
                            cx.place(StackPlacement {
                                align: Some(Alignment2D::End),
                                layer: 0,
                            });
                            leaf(cx, 20.0);
                        },
                    );
                    cx.absolute(
                        AbsoluteStyle {
                            size,
                            ..Default::default()
                        },
                        |cx| {
                            cx.place(AbsoluteInsets {
                                start: Some(Span::dp(5.0)),
                                ..AbsoluteInsets::NONE
                            });
                            leaf(cx, 20.0);
                            leaf(cx, 20.0);
                        },
                    );
                    let grid = GridStyle {
                        columns: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
                        rows: vec![TrackSizing::Fixed(20.0)],
                        size,
                        ..Default::default()
                    };
                    cx.grid(grid, |cx| {
                        leaf(cx, 20.0);
                        leaf(cx, 20.0);
                    });
                },
            );
            cx.root().unwrap()
        };
        store.set_box_rules(
            leaves[0],
            BoxRules {
                margin: SpanInset {
                    left: Span::dp(3.0),
                    ..SpanInset::ZERO
                },
                ..rules()
            },
        );
        let surface = surface_local(200.0, 300.0);
        let (mut scratch, mut redo) = (Vec::new(), Vec::new());
        store.layout(root, surface, &mut scratch);
        let xs = |store: &NodeStore| {
            leaves
                .iter()
                .map(|&leaf| store.bounds(leaf).x)
                .collect::<Vec<_>>()
        };
        let ltr = [3.0, 43.0, 0.0, 0.0, 180.0, 5.0, 0.0, 0.0, 50.0];
        assert_eq!(xs(&store), ltr);

        let mut env = store.length_env();
        env.direction = LayoutDirection::Rtl;
        store.clear_dirty();
        store.set_length_env(env);
        store.relayout_dirty(root, surface, &mut scratch, &mut redo);
        // The row runs from the right, its first child's left margin now
        // between it and the next; `start` is the right edge everywhere.
        assert_eq!(
            xs(&store),
            [170.0, 117.0, 150.0, 180.0, 0.0, 175.0, 180.0, 180.0, 130.0]
        );

        env.direction = LayoutDirection::Ltr;
        store.clear_dirty();
        store.set_length_env(env);
        store.relayout_dirty(root, surface, &mut scratch, &mut redo);
        assert_eq!(xs(&store), ltr);
    }

    #[test]
    fn a_node_s_own_direction_mirrors_its_subtree_alone() {
        use crate::adaptive::LayoutDirection;
        use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
        use crate::length::{EdgeTerms, LengthTerms, NodeLengths};

        let mut store = NodeStore::new();
        let mut rows = Vec::new();
        let mut leaves = Vec::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    size: Size::fixed(200.0, 100.0),
                    ..Default::default()
                },
                |cx| {
                    for _ in 0..2 {
                        let row = cx.flex(
                            FlexStyle {
                                axis: Axis::Row,
                                size: Size::fixed(200.0, 20.0),
                                ..Default::default()
                            },
                            |cx| {
                                leaves.push(
                                    cx.leaf(LeafStyle {
                                        size: Size::fixed(30.0, 10.0),
                                        ..Default::default()
                                    })
                                    .id(),
                                );
                            },
                        );
                        rows.push(row.id());
                    }
                },
            );
            cx.root().unwrap()
        };
        // The second row pads its start, which reads the direction.
        store.bind_lengths(
            rows[1],
            NodeLengths {
                padding: Some(EdgeTerms {
                    start: LengthTerms::dp(5.0),
                    ..EdgeTerms::default()
                }),
                ..NodeLengths::default()
            },
        );
        let surface = surface_local(200.0, 100.0);
        let (mut scratch, mut redo) = (Vec::new(), Vec::new());
        store.layout(root, surface, &mut scratch);
        let xs = |store: &NodeStore| [store.bounds(leaves[0]).x, store.bounds(leaves[1]).x];
        assert_eq!(xs(&store), [0.0, 5.0]);

        store.clear_dirty();
        store.set_layout_direction(rows[1], Some(LayoutDirection::Rtl));
        store.relayout_dirty(root, surface, &mut scratch, &mut redo);
        assert_eq!(xs(&store), [0.0, 165.0], "its start is the right edge");
        assert_eq!(store.layout_direction(leaves[1]), LayoutDirection::Rtl);
        assert_eq!(store.layout_direction(leaves[0]), LayoutDirection::Ltr);
        assert_eq!(store.scoped_direction(leaves[0]), None);

        // A right-to-left window, and the row back to left to right.
        let mut env = store.length_env();
        env.direction = LayoutDirection::Rtl;
        store.clear_dirty();
        store.set_length_env(env);
        store.set_layout_direction(rows[1], Some(LayoutDirection::Ltr));
        store.relayout_dirty(root, surface, &mut scratch, &mut redo);
        assert_eq!(xs(&store), [170.0, 5.0]);

        store.clear_dirty();
        store.set_layout_direction(rows[1], None);
        store.relayout_dirty(root, surface, &mut scratch, &mut redo);
        assert_eq!(xs(&store), [170.0, 165.0], "the window's again");
    }

    /// The boxes of leaves requesting `width`, each a text `min` to `max`
    /// wide and 10 high, in a 100 × 100 container along `axis`; and the
    /// container's natural extent once it fits them.
    fn texts(axis: Axis, leaves: &[(Length, f32, f32)]) -> (Vec<Rect>, Measured) {
        use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
        use crate::content::Content;
        use crate::style::BoxStyle;
        use viso_render::{Rgba, TextureId};

        let mut store = NodeStore::new();
        let mut ids = Vec::new();
        let (root, inner) = {
            let mut cx = BuildCx::new(&mut store);
            let mut inner = None;
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    size: Size::fixed(100.0, 100.0),
                    ..Default::default()
                },
                |cx| {
                    let fit = FlexStyle {
                        axis,
                        size: Size::fill(),
                        ..Default::default()
                    };
                    inner = Some(cx.flex(fit, |cx| {
                        for &(width, ..) in leaves {
                            let size = Size {
                                width,
                                height: Length::Fit,
                            };
                            let style = BoxStyle::NONE;
                            ids.push(cx.leaf(LeafStyle { size, style }).id());
                        }
                    }));
                },
            );
            (cx.root().unwrap(), inner.unwrap().id())
        };
        for (&id, &(_, min, max)) in ids.iter().zip(leaves) {
            let content = Content::Text {
                ink: None,
                glyphs: Vec::new(),
                atlas: TextureId::new(0),
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                natural: Vec2 { x: max, y: 10.0 },
                baseline: 8.0,
                shaped_at_width: None,
                width_use: crate::WidthUse::Ignored,
                min_content: min,
                max_content: max,
            };
            store.set_content_payload(id, content);
        }
        store.layout(root, surface_local(100.0, 100.0), &mut Vec::new());
        let boxes = ids.iter().map(|&id| store.bounds(id)).collect();
        (boxes, store.measured(inner.index()))
    }

    #[test]
    fn a_fit_width_takes_its_content_within_the_room_and_the_content_sizes_ignore_it() {
        let (boxes, column) = texts(
            Axis::Column,
            &[
                (Length::Fit, 40.0, 150.0),
                (Length::Fit, 120.0, 150.0),
                (Length::Fit, 20.0, 60.0),
                (Length::MinContent, 40.0, 150.0),
                (Length::MaxContent, 40.0, 150.0),
            ],
        );
        let widths: Vec<f32> = boxes.iter().map(|b| b.w).collect();
        assert_eq!(widths, [100.0, 120.0, 60.0, 40.0, 150.0]);
        // A column is as narrow as its widest child's narrowest, here the
        // max-content one's whole width.
        assert_eq!((column.w, column.min_w), (150.0, 150.0));
    }

    #[test]
    fn an_overflowing_row_shrinks_its_fit_children_alike_down_to_their_min_content() {
        // 80 + 40 + 20 overflow 100 by 40: the fits give up a third each, the
        // second stops at its 30 and the first gives up the rest.
        let (boxes, row) = texts(
            Axis::Row,
            &[
                (Length::Fit, 30.0, 80.0),
                (Length::Fit, 30.0, 40.0),
                (Length::Fixed(20.0), 0.0, 0.0),
            ],
        );
        let milli = |v: f32| (v * 1000.0).round() / 1000.0;
        let xs: Vec<(f32, f32)> = boxes.iter().map(|b| (milli(b.x), milli(b.w))).collect();
        assert_eq!(xs, [(0.0, 50.0), (50.0, 30.0), (80.0, 20.0)]);
        assert_eq!((row.w, row.min_w), (140.0, 80.0));

        // Past their min-content widths, they overflow.
        let (boxes, _) = texts(
            Axis::Row,
            &[(Length::Fit, 70.0, 80.0), (Length::MaxContent, 0.0, 50.0)],
        );
        let xs: Vec<(f32, f32)> = boxes.iter().map(|b| (b.x, b.w)).collect();
        assert_eq!(xs, [(0.0, 70.0), (70.0, 50.0)]);

        // A row with room for them leaves them be.
        let (boxes, _) = texts(Axis::Row, &[(Length::Fit, 10.0, 30.0)]);
        assert_eq!(boxes[0].w, 30.0);
    }

    #[test]
    fn auto_columns_too_wide_for_their_grid_shrink_toward_their_min_content() {
        use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
        use crate::content::Content;
        use crate::grid::GridStyle;
        use crate::style::BoxStyle;
        use viso_render::{Rgba, TextureId};

        // Texts 30 to 80 and 20 to 60 wide in a 100-wide grid of two `auto`
        // columns: each column starts at its min and the 50 left is shared.
        let mut store = NodeStore::new();
        let mut ids = Vec::new();
        let mut grid = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    size: Size::fixed(100.0, 100.0),
                    ..Default::default()
                },
                |cx| {
                    let style = GridStyle {
                        columns: vec![TrackSizing::Auto, TrackSizing::Auto],
                        size: Size {
                            width: Length::Fill { weight: 1.0 },
                            height: Length::Fit,
                        },
                        ..Default::default()
                    };
                    grid = Some(cx.grid(style, |cx| {
                        for _ in 0..2 {
                            let size = Size {
                                width: Length::Fit,
                                height: Length::Fit,
                            };
                            let style = BoxStyle::NONE;
                            ids.push(cx.leaf(LeafStyle { size, style }).id());
                        }
                    }));
                },
            );
            cx.root().unwrap()
        };
        for (&id, (min, max)) in ids.iter().zip([(30.0, 80.0), (20.0, 60.0)]) {
            store.set_content_payload(
                id,
                Content::Text {
                    ink: None,
                    glyphs: Vec::new(),
                    atlas: TextureId::new(0),
                    color_glyphs: Vec::new(),
                    color_atlas: None,
                    color: Rgba {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: 1.0,
                    },
                    natural: Vec2 { x: max, y: 10.0 },
                    baseline: 8.0,
                    shaped_at_width: None,
                    width_use: crate::WidthUse::Ignored,
                    min_content: min,
                    max_content: max,
                },
            );
        }
        store.layout(root, surface_local(100.0, 100.0), &mut Vec::new());
        let boxes: Vec<(f32, f32)> = ids
            .iter()
            .map(|&id| (store.bounds(id).x, store.bounds(id).w))
            .collect();
        assert_eq!(boxes, [(0.0, 55.0), (55.0, 45.0)]);
        // The grid's own content runs from its columns' minimums to their
        // max-content widths.
        let measured = store.measured(grid.unwrap().id().index());
        assert_eq!(
            (measured.w, measured.min_w, measured.h),
            (140.0, 50.0, 10.0)
        );
    }

    #[test]
    fn a_margin_counts_in_its_parents_natural_size() {
        use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
        use crate::style::BoxStyle;

        let mut store = NodeStore::new();
        let mut leaf = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    size: Size {
                        width: Length::Fit,
                        height: Length::Fit,
                    },
                    ..Default::default()
                },
                |cx| {
                    leaf = Some(
                        cx.leaf(LeafStyle {
                            size: Size::fixed(40.0, 20.0),
                            style: BoxStyle::NONE,
                        })
                        .id(),
                    );
                },
            );
            cx.root().unwrap()
        };
        store.set_box_rules(
            leaf.unwrap(),
            BoxRules {
                margin: SpanInset {
                    left: Span::dp(10.0),
                    top: Span::dp(5.0),
                    right: Span::dp(10.0),
                    bottom: Span::pct(50.0),
                },
                ..rules()
            },
        );
        measure(&mut store, root.index(), &mut Vec::new());
        // A percentage has no basis while measuring: it counts as nothing.
        let natural = Measured {
            w: 60.0,
            h: 25.0,
            min_w: 60.0,
        };
        assert_eq!(store.measured(root.index()), natural);
    }

    #[test]
    fn a_padding_percentage_reads_the_parent_content_box_and_survives_a_relayout() {
        use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
        use crate::style::BoxStyle;

        let mut store = NodeStore::new();
        let (mut panel, mut leaf) = (None, None);
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    size: Size::fill(),
                    ..Default::default()
                },
                |cx| {
                    panel = Some(
                        cx.flex(
                            FlexStyle {
                                axis: Axis::Column,
                                size: Size::fixed(100.0, 100.0),
                                ..Default::default()
                            },
                            |cx| {
                                leaf = Some(
                                    cx.leaf(LeafStyle {
                                        size: Size::fixed(10.0, 10.0),
                                        style: BoxStyle::NONE,
                                    })
                                    .id(),
                                );
                            },
                        )
                        .id(),
                    );
                },
            );
            cx.root().unwrap()
        };
        let (panel, leaf) = (panel.unwrap(), leaf.unwrap());
        store.set_box_rules(
            panel,
            BoxRules {
                padding_pct: Inset {
                    left: 0.1,
                    top: 0.1,
                    right: 0.0,
                    bottom: 0.0,
                },
                ..rules()
            },
        );
        let surface = surface_local(400.0, 300.0);
        store.layout(root, surface, &mut Vec::new());
        // 10% of the root's 400 wide content box, 10% of its 300 high one.
        assert_eq!(store.bounds(leaf), Rect::new(40.0, 30.0, 10.0, 10.0));

        store.clear_dirty();
        store.set_fixed_size(leaf, Size::fixed(20.0, 20.0));
        store.relayout_dirty(root, surface, &mut Vec::new(), &mut Vec::new());
        assert_eq!(store.bounds(leaf), Rect::new(40.0, 30.0, 20.0, 20.0));
    }

    #[test]
    fn the_root_keeps_its_margin_and_bounds_on_the_surface() {
        use crate::component::{BuildCx, FlexStyle, NodeStore};

        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |_| {});
            cx.root().unwrap()
        };
        store.set_box_rules(
            root,
            BoxRules {
                max_width: Span::dp(300.0),
                margin: SpanInset {
                    left: Span::dp(8.0),
                    top: Span::dp(8.0),
                    ..SpanInset::ZERO
                },
                ..rules()
            },
        );
        store.layout(root, surface_local(400.0, 200.0), &mut Vec::new());
        assert_eq!(store.bounds(root), Rect::new(8.0, 8.0, 300.0, 192.0));
    }

    #[test]
    fn a_relative_length_resolves_against_the_parent_content_box() {
        use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
        use crate::style::BoxStyle;

        // A 400x200 row with 10dp padding: the content box is 380x180. The first
        // child is `50%` wide and `100% - 20dp` tall; the second is a fill that
        // takes what the first leaves, proving `%` is a ratio and not a share.
        let mut store = NodeStore::new();
        let mut ids = Vec::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    align: Align::Start,
                    padding: Inset::all(10.0),
                    size: Size::fill(),
                    ..Default::default()
                },
                |cx| {
                    let rel = cx.leaf(LeafStyle {
                        size: Size {
                            width: Length::pct(50.0),
                            height: Length::Relative {
                                fixed: -20.0,
                                pct: 1.0,
                            },
                        },
                        style: BoxStyle::NONE,
                    });
                    let fill = cx.leaf(LeafStyle {
                        size: Size {
                            width: Length::fill(),
                            height: Length::dp(5.0),
                        },
                        style: BoxStyle::NONE,
                    });
                    ids.extend([rel.id(), fill.id()]);
                },
            );
            cx.root().unwrap()
        };
        let mut scratch = Vec::new();
        measure(&mut store, root.index(), &mut scratch);
        layout(
            &mut store,
            root.index(),
            surface_local(400.0, 200.0),
            &mut scratch,
        );
        let rel = store.bounds(ids[0]);
        let fill = store.bounds(ids[1]);
        assert_eq!((rel.x, rel.y, rel.w, rel.h), (10.0, 10.0, 190.0, 160.0));
        assert_eq!((fill.x, fill.w), (200.0, 190.0));
    }

    #[test]
    fn a_resolved_relative_length_is_never_negative() {
        let len = Length::Relative {
            fixed: -50.0,
            pct: 0.1,
        };
        assert_eq!(len.resolve(100.0), Some(0.0));
        assert_eq!(len.resolve(1000.0), Some(50.0));
        assert_eq!(Length::fill().resolve(100.0), None);
        assert_eq!(Length::Fit.resolve(100.0), None);
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
