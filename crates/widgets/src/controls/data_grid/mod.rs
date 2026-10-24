//! The [`DataGrid`] control — a table of rows and columns that scrolls both
//! ways and builds only the rows and the columns in view.
//!
//! A grid is a virtual list scrolling across too: its first row is the
//! column headers, pinned at the top as the rows pass under it, and each row
//! builds the cells of the columns its viewport shows across (and one each
//! side), building and freeing them as it scrolls across. A million rows of
//! a hundred columns mount a screenful of cells.
//!
//! Columns are dragged wider or narrower by the edge of their header, and to
//! another place by their header, live; a press on a sortable header sorts
//! by its column, again the other way. The grid shows a cursor cell and
//! selects rows or rectangles of cells: a press selects, Shift extends, the
//! primary modifier (Command on Apple systems, Control elsewhere) adds or,
//! on a selected row, drops; a drag selects as it goes. The keyboard moves
//! the cursor (arrows, Home/End, Page Up/Down, with the primary modifier to
//! the edges), Shift extending; Space toggles the cursor's row, the primary
//! modifier with A selects all and with C copies the selection as
//! tab-separated text; Enter or F2 activates the cursor's cell (to edit it).
//! On a header, Enter or Space sorts, the primary modifier and Shift with
//! Left/Right moves its column, Alt with Left/Right resizes it. A double
//! press activates a cell. Read right to left the columns run from the
//! right, and Left/Right follow the screen.
//!
//! The grid is one tab stop, `grid` to an assistive technology, telling its
//! full row and column counts; rows are `row`, headers `columnheader` with
//! their sort, cells `gridcell` with their selection, each saying its row
//! and column so the cells not built still count; the cursor's cell is the
//! active descendant.
//!
//! ```
//! use viso_widgets::{DataGridHandleSlot, data_grid, grid_column};
//! use viso_ui::{BindingTable, BuildCx, Component, NodeStore, SemanticProjector, StateStore, TextEdits, VirtualLists};
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! let slot = DataGridHandleSlot::default();
//! data_grid(
//!     "Orders",
//!     10_000,
//!     vec![
//!         grid_column("Id").width(80.0).sortable(),
//!         grid_column("Customer").width(200.0).sortable(),
//!         grid_column("Total").width(100.0).align_end(),
//!     ],
//! )
//! .text(|row, column| format!("{row}:{column}"))
//! .on_sort(|_ev, column, order| { /* sort the data */ })
//! .handle(&slot)
//! .build(&mut cx);
//! ```

mod cells;
mod columns;
mod input;
mod selection;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use viso_ui::virtual_list::{set_sticky_of, watch_across};
use viso_ui::{
    AcrossView, BoxStyle, BuildCx, Component, EventCx, Justify, NodeId, Rgba, Role, Semantics,
    Size, SortOrder, StateId, StateValue, TextAlign, VirtualListStyle,
};

pub use selection::{GridRange, GridSelect, GridSelection};

use selection::Selection;

/// A column of a [`DataGrid`]. Construct one with [`grid_column`].
#[derive(Debug, Clone, PartialEq)]
pub struct GridColumn {
    title: String,
    width: f32,
    min: f32,
    max: f32,
    sortable: bool,
    resizable: bool,
    align: TextAlign,
}

/// A column headed `title`, 120 wide, resizable from 40 to 2000.
pub fn grid_column(title: impl Into<String>) -> GridColumn {
    GridColumn {
        title: title.into(),
        width: 120.0,
        min: 40.0,
        max: 2000.0,
        sortable: false,
        resizable: true,
        align: TextAlign::Start,
    }
}

impl GridColumn {
    /// Make it `width` wide.
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// Let it be resized no narrower than `min`.
    pub fn min(mut self, min: f32) -> Self {
        self.min = min;
        self
    }

    /// Let it be resized no wider than `max`.
    pub fn max(mut self, max: f32) -> Self {
        self.max = max;
        self
    }

    /// Let a press on its header sort by it.
    pub fn sortable(mut self) -> Self {
        self.sortable = true;
        self
    }

    /// Keep its width.
    pub fn fixed(mut self) -> Self {
        self.resizable = false;
        self
    }

    /// Set its cells' text at their end edge: numbers.
    pub fn align_end(self) -> Self {
        self.align(TextAlign::End)
    }

    /// Set where its cells' text sits.
    pub fn align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }

    fn clamp(&self, width: f32) -> f32 {
        width.clamp(self.min, self.max.max(self.min))
    }

    fn justify(&self) -> Justify {
        match self.align {
            TextAlign::End => Justify::End,
            TextAlign::Center => Justify::Center,
            _ => Justify::Start,
        }
    }
}

/// How a [`DataGrid`] looks. Logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DataGridStyle {
    /// The grid's own size within its parent.
    pub size: Size,
    /// A row's height and the headers'.
    pub row_height: f32,
    pub header_height: f32,
    /// The room at a cell's start and end edges.
    pub padding: f32,
    pub font_size: f32,
    /// The text's color, and the headers'.
    pub text: Rgba,
    pub header_text: Rgba,
    /// What shows behind the rows, behind the headers, and behind every
    /// other row.
    pub background: Rgba,
    pub header_fill: Rgba,
    pub stripe: Rgba,
    /// The row under the pointer.
    pub hover: Rgba,
    /// The line under each row, under the headers, and between headers.
    pub line: Rgba,
    pub header_line: Rgba,
    /// The selection while the grid has the focus, and while it has not.
    pub selection: Rgba,
    pub selection_idle: Rgba,
    /// The cursor cell's frame while the grid has the focus, and its width.
    pub cursor: Rgba,
    pub cursor_width: f32,
}

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba { r, g, b, a }
}

impl Default for DataGridStyle {
    fn default() -> Self {
        DataGridStyle {
            size: Size::fill(),
            row_height: 28.0,
            header_height: 32.0,
            padding: 8.0,
            font_size: 13.0,
            text: rgba(0.12, 0.13, 0.15, 1.0),
            header_text: rgba(0.3, 0.32, 0.36, 1.0),
            background: rgba(1.0, 1.0, 1.0, 1.0),
            header_fill: rgba(0.96, 0.965, 0.97, 1.0),
            stripe: rgba(0.0, 0.0, 0.0, 0.025),
            hover: rgba(0.0, 0.0, 0.0, 0.045),
            line: rgba(0.0, 0.0, 0.0, 0.07),
            header_line: rgba(0.0, 0.0, 0.0, 0.14),
            selection: rgba(0.2, 0.47, 0.96, 0.18),
            selection_idle: rgba(0.0, 0.0, 0.0, 0.08),
            cursor: rgba(0.2, 0.47, 0.96, 1.0),
            cursor_width: 2.0,
        }
    }
}

/// What builds a data cell's body, given its data row and its column as
/// declared.
pub type CellBuilder = Rc<dyn Fn(&mut BuildCx<'_>, usize, usize)>;
/// A data cell's text, given its data row and its column as declared.
pub type CellText = Rc<dyn Fn(usize, usize) -> String>;

type OnSelect = Box<dyn FnMut(&mut EventCx<'_>, &GridSelection)>;
type OnSort = Box<dyn FnMut(&mut EventCx<'_>, usize, SortOrder)>;
type OnCell = Box<dyn FnMut(&mut EventCx<'_>, usize, usize)>;
type OnResize = Box<dyn FnMut(&mut EventCx<'_>, usize, f32)>;
type OnReorder = Box<dyn FnMut(&mut EventCx<'_>, &[usize])>;

/// A table of rows and columns. Construct one with [`data_grid`].
///
/// Invalidation: scrolling moves the rows by their transform and, crossing a
/// row or a column, mounts and frees cells before the frame's layout; a
/// selection, cursor or hover change restyles the cells built (`PAINT`,
/// `SEMANTICS`); a resize resizes them and re-lays the rows out (`MEASURE |
/// LAYOUT`); a reorder, a sort or a refresh rebuilds the cells built.
pub struct DataGrid {
    label: String,
    rows: usize,
    columns: Vec<GridColumn>,
    style: DataGridStyle,
    select: GridSelect,
    cell: Option<CellBuilder>,
    text: Option<CellText>,
    // Handlers move into the grid it builds: `build` takes `&self`.
    on_select: RefCell<Option<OnSelect>>,
    on_sort: RefCell<Option<OnSort>>,
    on_activate: RefCell<Option<OnCell>>,
    on_resize: RefCell<Option<OnResize>>,
    on_reorder: RefCell<Option<OnReorder>>,
    slot: Option<DataGridHandleSlot>,
}

/// A grid named `label` of `rows` data rows under `columns`.
pub fn data_grid(label: impl Into<String>, rows: usize, columns: Vec<GridColumn>) -> DataGrid {
    DataGrid {
        label: label.into(),
        rows,
        columns,
        style: DataGridStyle::default(),
        select: GridSelect::default(),
        cell: None,
        text: None,
        on_select: RefCell::new(None),
        on_sort: RefCell::new(None),
        on_activate: RefCell::new(None),
        on_resize: RefCell::new(None),
        on_reorder: RefCell::new(None),
        slot: None,
    }
}

impl DataGrid {
    /// Show `text(row, column)` in each data cell; it is also what a copy
    /// copies.
    pub fn text(mut self, text: impl Fn(usize, usize) -> String + 'static) -> Self {
        self.text = Some(Rc::new(text));
        self
    }

    /// Build each data cell's body with `cell(cx, row, column)`, in place of
    /// its text.
    pub fn cell(mut self, cell: impl Fn(&mut BuildCx<'_>, usize, usize) + 'static) -> Self {
        self.cell = Some(Rc::new(cell));
        self
    }

    /// Select as `select` says: rows (the default), cells or nothing.
    pub fn select(mut self, select: GridSelect) -> Self {
        self.select = select;
        self
    }

    /// Run `handler` with the selection each time it changes.
    pub fn on_select(
        mut self,
        handler: impl FnMut(&mut EventCx<'_>, &GridSelection) + 'static,
    ) -> Self {
        self.on_select = RefCell::new(Some(Box::new(handler)));
        self
    }

    /// Run `handler` with a column (as declared) and the order to sort the
    /// data by it when its header asks; the grid shows the order on the
    /// header and rebuilds the cells after.
    pub fn on_sort(
        mut self,
        handler: impl FnMut(&mut EventCx<'_>, usize, SortOrder) + 'static,
    ) -> Self {
        self.on_sort = RefCell::new(Some(Box::new(handler)));
        self
    }

    /// Run `handler` with a data row and a column (as declared) when the
    /// cell is activated: Enter, F2 or a double press, to edit it.
    pub fn on_activate(
        mut self,
        handler: impl FnMut(&mut EventCx<'_>, usize, usize) + 'static,
    ) -> Self {
        self.on_activate = RefCell::new(Some(Box::new(handler)));
        self
    }

    /// Run `handler` with a column (as declared) and its width once it was
    /// resized.
    pub fn on_resize(
        mut self,
        handler: impl FnMut(&mut EventCx<'_>, usize, f32) + 'static,
    ) -> Self {
        self.on_resize = RefCell::new(Some(Box::new(handler)));
        self
    }

    /// Run `handler` with the columns (as declared) in their new order on
    /// screen once one was moved.
    pub fn on_reorder(mut self, handler: impl FnMut(&mut EventCx<'_>, &[usize]) + 'static) -> Self {
        self.on_reorder = RefCell::new(Some(Box::new(handler)));
        self
    }

    /// Replace the whole [`DataGridStyle`].
    pub fn style(mut self, style: DataGridStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the grid's own size within its parent.
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// Fill `slot` with the grid's handle once it is built.
    pub fn handle(mut self, slot: &DataGridHandleSlot) -> Self {
        self.slot = Some(Rc::clone(slot));
        self
    }
}

/// What a frame's structure pass applies, set by the handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Pending(u8);

impl Pending {
    /// Restyle the cells built for the selection, the cursor and the hover.
    const LOOKS: Pending = Pending(1);
    /// Resize the cells built and re-place the columns.
    const WIDTHS: Pending = Pending(1 << 1);
    /// Rebuild every cell built: the data, or the columns' order, changed.
    const CELLS: Pending = Pending(1 << 2);
    /// Rebuild the headers: the sort changed.
    const HEADERS: Pending = Pending(1 << 3);
    /// Take the row count anew.
    const COUNT: Pending = Pending(1 << 4);
    /// Scroll the cursor's cell into view.
    const REVEAL: Pending = Pending(1 << 5);

    fn has(self, other: Pending) -> bool {
        self.0 & other.0 != 0
    }
}

impl std::ops::BitOr for Pending {
    type Output = Pending;

    fn bitor(self, other: Pending) -> Pending {
        Pending(self.0 | other.0)
    }
}

/// What a pointer pressed on the grid is doing.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Gesture {
    None,
    /// Resizing the column at place `place`, from `width` with the pointer
    /// `from` along.
    Resize {
        place: usize,
        from: f32,
        width: f32,
    },
    /// Pressed on the header at `place`, `from` along; `moved` once it was
    /// dragged past the slop, the column following.
    Header {
        place: usize,
        from: f32,
        moved: bool,
    },
    /// Selecting from a press on a cell.
    Select,
    /// A finger down on cell `at`: a lift without a pan selects it.
    Tap {
        at: (usize, usize),
        extend: bool,
        add: bool,
    },
}

/// A built grid's nodes and the cell its handlers write.
#[derive(Debug, Clone, Copy)]
struct Built {
    viewport: NodeId,
    canvas: NodeId,
    revision: StateId,
}

/// A row's nodes and the cells built in it.
#[derive(Debug, Clone)]
struct RowCells {
    /// The list row it shows: `0` the headers, else the data row past it.
    logical: usize,
    row: NodeId,
    /// The strip the cells lie in, after the spacer that stands for the
    /// columns before them.
    strip: NodeId,
    spacer: NodeId,
    /// The place on screen of the first cell built, and the cells.
    first: usize,
    cells: std::collections::VecDeque<NodeId>,
}

/// A grid's state, shared by its handlers, its structure hook and its
/// handle.
struct Grid {
    style: DataGridStyle,
    select: GridSelect,
    cell: Option<CellBuilder>,
    text: Option<CellText>,
    columns: RefCell<Vec<GridColumn>>,
    /// The column each place on screen shows.
    order: RefCell<Vec<usize>>,
    /// Where each place on screen starts, the total last.
    starts: RefCell<Vec<f32>>,
    rows: Cell<usize>,
    /// What the viewport shows across, and the places built.
    view: Cell<AcrossView>,
    window: Cell<(usize, usize)>,
    /// The cursor: a list row (`0` the headers) and a place on screen.
    cursor: Cell<(usize, usize)>,
    selection: RefCell<Selection>,
    sort: Cell<Option<(usize, SortOrder)>>,
    /// The list row under the pointer.
    hovered: Cell<Option<usize>>,
    focused: Cell<bool>,
    gesture: Cell<Gesture>,
    /// Each mounted row's cells, by its list host.
    built_rows: RefCell<HashMap<NodeId, RowCells>>,
    built: Cell<Option<Built>>,
    pending: Cell<Pending>,
    on_select: RefCell<Option<OnSelect>>,
    on_sort: RefCell<Option<OnSort>>,
    on_activate: RefCell<Option<OnCell>>,
    on_resize: RefCell<Option<OnResize>>,
    on_reorder: RefCell<Option<OnReorder>>,
}

impl Grid {
    fn column_count(&self) -> usize {
        self.order.borrow().len()
    }

    /// The column place `place` on screen shows.
    fn model(&self, place: usize) -> usize {
        self.order.borrow()[place]
    }

    /// Re-place the columns after a width or the order changed.
    fn place_columns(&self) {
        let columns = self.columns.borrow();
        *self.starts.borrow_mut() = columns::starts(&self.order.borrow(), |m| columns[m].width);
    }

    fn total_width(&self) -> f32 {
        self.starts.borrow().last().copied().unwrap_or(0.0)
    }

    /// Ask the next structure pass to apply `pending`.
    fn ask(&self, ev: &mut EventCx<'_>, pending: Pending) {
        let Some(built) = self.built.get() else {
            return;
        };
        self.pending.set(self.pending.get() | pending);
        let n = match ev.get(built.revision) {
            Some(StateValue::Int(n)) => n,
            _ => 0,
        };
        ev.set(built.revision, StateValue::Int(n.wrapping_add(1)));
    }

    /// Tell the select handler the selection.
    fn tell_selection(&self, ev: &mut EventCx<'_>) {
        let told = self
            .selection
            .borrow()
            .told(self.select, &self.order.borrow());
        let handler = self.on_select.borrow_mut().take();
        if let Some(mut handler) = handler {
            handler(ev, &told);
            restore(&self.on_select, handler);
        }
    }
}

/// Put `handler` back in `slot` unless the handler set another.
fn restore<T>(slot: &RefCell<Option<T>>, handler: T) {
    let mut slot = slot.borrow_mut();
    if slot.is_none() {
        *slot = Some(handler);
    }
}

/// A built [`DataGrid`], to change its data and read what it shows. Cheap
/// to clone: every clone is the same grid.
#[derive(Clone)]
pub struct DataGridHandle(Rc<Grid>);

/// Where a [`DataGrid`] puts its handle once built.
pub type DataGridHandleSlot = Rc<RefCell<Option<DataGridHandle>>>;

impl DataGridHandle {
    /// The data rows it has.
    pub fn rows(&self) -> usize {
        self.0.rows.get()
    }

    /// Give it `rows` data rows, rebuilding the cells built: the data
    /// changed. A selection past the last row is dropped.
    pub fn set_rows(&self, ev: &mut EventCx<'_>, rows: usize) {
        let grid = &self.0;
        grid.rows.set(rows);
        grid.selection.borrow_mut().clamp(rows);
        let (row, place) = grid.cursor.get();
        grid.cursor.set((row.min(rows), place));
        grid.ask(ev, Pending::COUNT | Pending::CELLS);
    }

    /// Rebuild the cells built: the data they show changed.
    pub fn refresh(&self, ev: &mut EventCx<'_>) {
        self.0.ask(ev, Pending::CELLS);
    }

    /// What it has selected.
    pub fn selection(&self) -> GridSelection {
        let grid = &self.0;
        grid.selection
            .borrow()
            .told(grid.select, &grid.order.borrow())
    }

    /// Select nothing.
    pub fn clear_selection(&self, ev: &mut EventCx<'_>) {
        self.0.selection.borrow_mut().clear();
        self.0.ask(ev, Pending::LOOKS);
    }

    /// The cursor's data row (`None` on the headers) and column as declared.
    pub fn cursor(&self) -> (Option<usize>, usize) {
        let (row, place) = self.0.cursor.get();
        let column = self.0.order.borrow().get(place).copied().unwrap_or(0);
        (row.checked_sub(1), column)
    }

    /// Show the grid sorted by `column` (as declared) in `order`, or by no
    /// column: after sorting the data some other way.
    pub fn show_sort(&self, ev: &mut EventCx<'_>, sort: Option<(usize, SortOrder)>) {
        self.0.sort.set(sort);
        self.0.ask(ev, Pending::HEADERS);
    }

    /// The columns (as declared) in their order on screen.
    pub fn order(&self) -> Vec<usize> {
        self.0.order.borrow().clone()
    }

    /// Column `column`'s (as declared) width.
    pub fn width(&self, column: usize) -> Option<f32> {
        self.0.columns.borrow().get(column).map(|c| c.width)
    }
}

impl Component for DataGrid {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let style = self.style;
        let grid = Rc::new(Grid {
            style,
            select: self.select,
            cell: self.cell.clone(),
            text: self.text.clone(),
            columns: RefCell::new(self.columns.clone()),
            order: RefCell::new((0..self.columns.len()).collect()),
            starts: RefCell::new(Vec::new()),
            rows: Cell::new(self.rows),
            view: Cell::new(AcrossView {
                offset: 0.0,
                extent: 0.0,
            }),
            window: Cell::new((0, 0)),
            cursor: Cell::new((usize::from(self.rows > 0), 0)),
            selection: RefCell::new(Selection::default()),
            sort: Cell::new(None),
            hovered: Cell::new(None),
            focused: Cell::new(false),
            gesture: Cell::new(Gesture::None),
            built_rows: RefCell::new(HashMap::new()),
            built: Cell::new(None),
            pending: Cell::new(Pending::default()),
            on_select: RefCell::new(self.on_select.borrow_mut().take()),
            on_sort: RefCell::new(self.on_sort.borrow_mut().take()),
            on_activate: RefCell::new(self.on_activate.borrow_mut().take()),
            on_resize: RefCell::new(self.on_resize.borrow_mut().take()),
            on_reorder: RefCell::new(self.on_reorder.borrow_mut().take()),
        });
        grid.place_columns();
        grid.window
            .set(columns::window(&grid.starts.borrow(), grid.view.get()));
        let revision = cx.state(StateValue::Int(0));
        let rows_grid = Rc::clone(&grid);
        let viewport = cx.virtual_list(
            VirtualListStyle {
                across: true,
                size: style.size,
                estimated_row: style.row_height,
                style: BoxStyle::solid(style.background),
                ..VirtualListStyle::default()
            },
            self.rows + 1,
            move |logical, cx| cells::build_row(&rows_grid, logical, cx),
        );
        cx.focusable(viewport, true);
        cx.count_clicks(viewport);
        cx.semantics(
            viewport,
            Semantics::role(Role::Grid).with_label(self.label.clone()),
        );
        let pointer = Rc::clone(&grid);
        cx.on_pointer(viewport, move |ev| input::pointer(&pointer, ev));
        let keys = Rc::clone(&grid);
        cx.on_key(viewport, move |ev| input::key(&keys, ev));
        let watch = Rc::clone(&grid);
        let hook = Rc::clone(&grid);
        let viewport = viewport.id();
        cx.structure(|s| {
            let canvas = s.lists.get(viewport).map_or(viewport, |list| list.canvas());
            grid.built.set(Some(Built {
                viewport,
                canvas,
                revision,
            }));
            cells::tell_extent(&grid, s.store);
            s.store.set_list_across(viewport, grid.total_width());
            set_sticky_of(s.lists, viewport, Some(Box::new(|row| row == 0)));
            watch_across(
                s.lists,
                viewport,
                Box::new(move |s, view, rows| cells::seen_across(&watch, s, view, rows)),
            );
            s.store.add_structure_hook([revision], move |s, _| {
                if s.store.arena().is_live(viewport) {
                    cells::apply(&hook, s);
                }
            });
        });
        if let Some(slot) = &self.slot {
            *slot.borrow_mut() = Some(DataGridHandle(Rc::clone(&grid)));
        }
    }
}
