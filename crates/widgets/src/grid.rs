//! The [`Grid`] widget — a two-dimensional track-based layout container.
//!
//! `Grid` places its children into the cells of a column/row track grid: you
//! declare a template of column tracks and row tracks (each `Fixed`, `Fr`,
//! `Auto`, or `Percent`), a gap between them, and padding, and the children fall
//! into cells — row-major auto-flow by default, or pinned to an explicit cell
//! with [`Grid::place`]. It is the widget-layer face of the `viso-ui` Grid layout
//! node: it maps to exactly one Grid node and lowers straight onto the live
//! `BuildCx::grid`/`BuildCx::place` seam, so the whole `viso-ui` track-solving
//! pass (fixed/percent/auto sizing, then a re-normalizing fractional sweep over
//! the free space) handles a widget-authored grid unchanged.
//!
//! It differs from [`View`](crate::View) on purpose. `View` is a one-dimensional
//! flex box: children stack along a single axis. `Grid` is two-dimensional: a
//! child occupies a cell (or a span of cells) in a column×row template, so
//! columns and rows are sized together. Reach for `Grid` when you are laying out
//! a real table/dashboard/form grid; reach for `View` for a simple row or column.
//!
//! Its children are declared by a builder closure that runs against the same
//! [`BuildCx`], so cells and nested widgets attach beneath the grid with no
//! intermediate allocation. Inside the closure, call [`BuildCx::place`] (re-exported
//! here as the `place` seam via [`GridPlacement`]) before a child to pin it to a
//! cell or give it a span; a child authored without a preceding `place` auto-flows
//! into the next free cell with span 1. The default accessible role is
//! [`Role::Group`] — a structural grouping with no interaction of its own.

use viso_ui::{BoxStyle, BuildCx, Component, GridStyle, Inset, Role, Semantics, Size, TrackSizing};

/// The visual and layout parameters of a [`Grid`]: its column and row track
/// templates, the gaps between tracks, inner padding, the sizing of implicit
/// rows created by auto-flow past the template, the grid's own size within its
/// parent, and an optional background/border for the grid box itself.
///
/// This is a small, flat description of "a box with a track grid and a
/// background": the fields of the underlying `viso-ui` Grid node, grouped for an
/// app author. Its [`Default`] is an empty grid (no tracks) with `Auto` implicit
/// rows, no gaps, fill size, and a transparent box — build one up by setting
/// [`columns`](GridViewStyle::columns)/[`rows`](GridViewStyle::rows).
#[derive(Debug, Clone, PartialEq)]
pub struct GridViewStyle {
    /// The column track template, outer to inner. Each track is a
    /// [`TrackSizing`] (`Fixed`, `Fr`, `Auto`, or `Percent`). An empty template
    /// is treated as a single implicit column.
    pub columns: Vec<TrackSizing>,
    /// The explicit row track template, top to bottom. Rows past this template
    /// (created by auto-flow) are sized by [`auto_rows`](GridViewStyle::auto_rows).
    pub rows: Vec<TrackSizing>,
    /// The sizing of implicit rows created when auto-flow places children past
    /// the explicit [`rows`](GridViewStyle::rows) template.
    pub auto_rows: TrackSizing,
    /// The gap between adjacent columns.
    pub column_gap: f32,
    /// The gap between adjacent rows.
    pub row_gap: f32,
    /// Inner padding on all four edges, inside the grid box, outside the tracks.
    pub padding: Inset,
    /// The grid's own size request within its parent.
    pub size: Size,
    /// The grid's background/border/radius. [`BoxStyle::NONE`] is a pure,
    /// transparent layout box.
    pub background: BoxStyle,
}

impl Default for GridViewStyle {
    fn default() -> Self {
        GridViewStyle {
            columns: Vec::new(),
            rows: Vec::new(),
            auto_rows: TrackSizing::Auto,
            column_gap: 0.0,
            row_gap: 0.0,
            padding: Inset::default(),
            size: Size::fill(),
            background: BoxStyle::NONE,
        }
    }
}

/// A [`Grid`]'s child-declaration closure. Boxed `Fn` (not `FnOnce`) because
/// [`Component::build`] takes `&self`: the widget is built by reference, so its
/// child builder is invoked through a shared borrow. Cold — run once at build
/// time, never on the per-frame hot path. Parallels `viso_ui`'s boxed handler
/// aliases (a cold fat pointer kept off the hot node columns).
type ChildBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// A two-dimensional track-based layout container: children fall into the cells
/// of a column×row grid, auto-flowing row-major or pinned to an explicit cell.
///
/// Construct one with [`grid`] and attach children with [`Grid::children`]. Use
/// [`BuildCx::place`] inside the closure to pin a child to a cell or span:
///
/// ```
/// use viso_widgets::{grid, GridViewStyle};
/// use viso_ui::{BuildCx, Component, GridPlacement, NodeStore, Size, TrackSizing};
///
/// let dashboard = grid(GridViewStyle {
///     columns: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
///     rows: vec![TrackSizing::Fixed(40.0)],
///     column_gap: 8.0,
///     row_gap: 8.0,
///     size: Size::fixed(200.0, 80.0),
///     ..Default::default()
/// })
/// .children(|cx| {
///     // auto-flows into cell (col 0, row 0)
///     cx.leaf(Default::default());
///     // pinned to column 1, spanning both columns of the next row
///     cx.place(GridPlacement { column: Some(0), row: Some(1), column_span: 2, row_span: 1 });
///     cx.leaf(Default::default());
/// });
///
/// let mut store = NodeStore::new();
/// let mut cx = BuildCx::new(&mut store);
/// dashboard.build(&mut cx);
/// ```
///
/// Invalidation: `Grid` declares only structure/layout/paint through the Grid
/// node it maps to; it holds no reactive state of its own, so a change to the
/// grid is expressed by rebuilding it. Semantics default to [`Role::Group`].
pub struct Grid {
    style: GridViewStyle,
    /// The child-declaration closure, if any. See [`ChildBuilder`].
    children: Option<ChildBuilder>,
    /// An optional accessible label. `None` keeps the plain [`Role::Group`]
    /// container with no name.
    label: Option<String>,
}

/// Construct a [`Grid`] with the given style and no children yet. Chain
/// [`Grid::children`] to author its cells and [`Grid::label`] to give it an
/// accessible name.
pub fn grid(style: GridViewStyle) -> Grid {
    Grid {
        style,
        children: None,
        label: None,
    }
}

impl Default for Grid {
    fn default() -> Self {
        grid(GridViewStyle::default())
    }
}

impl Grid {
    /// Attach a child-declaration closure. The closure runs with this grid as the
    /// active parent, so children authored inside it fall into cells. Call
    /// [`BuildCx::place`] before a child to pin it to a cell or give it a span;
    /// an un-placed child auto-flows into the next free cell with span 1.
    pub fn children(mut self, children: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.children = Some(Box::new(children));
        self
    }

    /// Give the grid an accessible label, surfaced on its [`Role::Group`]
    /// semantics node.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl Component for Grid {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // `grid` takes `FnOnce(&mut BuildCx)`. `self.children` is a shared `Fn`
        // (see the field note); wrap it so the borrow is invoked through the
        // closure. `|_cx| {}` when there are no children keeps the grid's own
        // node without adding an empty child. `GridStyle` owns its track
        // vectors, so the templates are cloned into it (build-time, cold path).
        let handle = cx.grid(
            GridStyle {
                columns: self.style.columns.clone(),
                rows: self.style.rows.clone(),
                auto_rows: self.style.auto_rows,
                column_gap: self.style.column_gap,
                row_gap: self.style.row_gap,
                padding: self.style.padding,
                size: self.style.size,
                style: self.style.background,
                // Named lines / template areas are a facade-level authoring
                // feature not surfaced through this widget; leave them empty.
                ..Default::default()
            },
            |cx| {
                if let Some(children) = &self.children {
                    children(cx);
                }
            },
        );

        // A grid container is a `Group` to an assistive technology; carry the
        // label when one was authored.
        let mut semantics = Semantics::role(Role::Group);
        if let Some(label) = &self.label {
            semantics = semantics.with_label(label.clone());
        }
        cx.semantics(handle, semantics);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{GridPlacement, LeafStyle, NodeId, NodeStore, Rect, Rgba};

    /// Count a node's direct children by walking the arena sibling chain.
    fn child_count(store: &NodeStore, parent: NodeId) -> usize {
        let arena = store.arena();
        let mut n = 0;
        let mut child = arena.links(parent).and_then(|l| l.first_child);
        while let Some(c) = child {
            n += 1;
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        n
    }

    /// A `Grid` maps to a single Grid node carrying the requested background, and
    /// its child closure runs beneath it.
    #[test]
    fn grid_builds_a_grid_node_with_background_and_children() {
        let bg = BoxStyle::solid(Rgba {
            r: 0.2,
            g: 0.2,
            b: 0.2,
            a: 1.0,
        });
        let g = grid(GridViewStyle {
            columns: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
            rows: vec![TrackSizing::Fixed(50.0)],
            background: bg,
            ..Default::default()
        })
        .children(|cx| {
            cx.leaf(LeafStyle::default());
            cx.leaf(LeafStyle::default());
        });

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        g.build(&mut cx);
        let root = cx.root().expect("grid declares a root node");

        assert!(
            store.is_grid(root),
            "a Grid widget maps to a grid layout node"
        );
        assert_eq!(
            child_count(&store, root),
            2,
            "the child closure's two cells attach beneath the grid"
        );
        assert_eq!(
            store.style(root),
            bg,
            "the grid carries the requested background"
        );
    }

    /// Two children with no explicit placement auto-flow row-major into the two
    /// columns of the template: the first lands in column 0, the second in
    /// column 1 of the same row.
    #[test]
    fn unplaced_children_auto_flow_row_major() {
        let g = grid(GridViewStyle {
            columns: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
            rows: vec![TrackSizing::Fixed(50.0)],
            size: Size::fixed(100.0, 50.0),
            ..Default::default()
        })
        .children(|cx| {
            cx.leaf(LeafStyle {
                size: Size::fill(),
                ..Default::default()
            });
            cx.leaf(LeafStyle {
                size: Size::fill(),
                ..Default::default()
            });
        });

        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            g.build(&mut cx);
            cx.root().expect("grid declares a root node")
        };

        let arena = store.arena();
        let first = arena.links(root).and_then(|l| l.first_child).unwrap();
        let second = arena.links(first).and_then(|l| l.next_sibling).unwrap();

        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 50.0,
            },
            &mut scratch,
        );

        // First child in column 0 (x = 0), second in column 1 (x = 50).
        assert_eq!(
            store.bounds(first).x,
            0.0,
            "first child auto-flows to column 0"
        );
        assert_eq!(
            store.bounds(second).x,
            50.0,
            "second child auto-flows to column 1"
        );
    }

    /// A `cx.place` call before a child pins it to an explicit cell: placing a
    /// child in column 1 lands it at the second column's origin.
    #[test]
    fn place_pins_a_child_to_an_explicit_cell() {
        let g = grid(GridViewStyle {
            columns: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
            rows: vec![TrackSizing::Fixed(50.0)],
            size: Size::fixed(100.0, 50.0),
            ..Default::default()
        })
        .children(|cx| {
            cx.place(GridPlacement {
                column: Some(1),
                row: Some(0),
                column_span: 1,
                row_span: 1,
            });
            cx.leaf(LeafStyle {
                size: Size::fill(),
                ..Default::default()
            });
        });

        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            g.build(&mut cx);
            cx.root().expect("grid declares a root node")
        };
        let child = store
            .arena()
            .links(root)
            .and_then(|l| l.first_child)
            .unwrap();

        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 50.0,
            },
            &mut scratch,
        );

        assert_eq!(
            store.bounds(child),
            Rect {
                x: 50.0,
                y: 0.0,
                w: 50.0,
                h: 50.0,
            },
            "the placed child sits in column 1 at the cell's full extent"
        );
    }

    /// The default accessible role of a grid is `Group`, an authored label
    /// surfaces on that node, and a grid declares no interactive handlers of its
    /// own (its children carry any interaction).
    #[test]
    fn grid_default_role_is_group_and_carries_label() {
        let g = grid(GridViewStyle::default()).label("Dashboard");

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        g.build(&mut cx);
        let root = cx.root().expect("grid declares a root node");

        let sem = store
            .semantics(root)
            .expect("grid authors semantics on its node");
        assert_eq!(sem.role, Role::Group);
        assert_eq!(sem.label.as_deref(), Some("Dashboard"));
        assert!(
            !store.has_handler(root) && !store.has_key_handler(root),
            "a Grid declares no interactive handlers of its own"
        );
    }
}
