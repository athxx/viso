//! A grid's rows and the cells in them: built as the list mounts a row,
//! the columns in view built and freed as it scrolls across, restyled for
//! the selection and the cursor, resized and rebuilt as its handlers ask.

use std::collections::VecDeque;

use viso_ui::virtual_list::set_item_count;
use viso_ui::{
    AcrossView, Align, Axis, Border, BoxStyle, BuildCx, Component, CursorIcon, DirtyClass,
    FlexStyle, FontWeight, Handle, Inset, Justify, LeafStyle, Length, NodeId, NodeStore, Rgba,
    Role, SemanticState, Semantics, Size, SortOrder, StructureCx, TableFacts, Vec2,
};

use super::{Grid, GridSelect, Pending, RowCells, columns};
use crate::text::label;
use viso_ui::glyphs::{chevron_down, chevron_up, glyph_slot};

/// The line under each row.
const LINE: f32 = 1.0;
/// How wide a header's resize grip is, at its end edge.
pub(super) const GRIP: f32 = 8.0;

/// List row `logical`'s height, its line under it included.
pub(super) fn height(grid: &Grid, logical: usize) -> f32 {
    if logical == 0 {
        grid.style.header_height
    } else {
        grid.style.row_height
    }
}

fn place_facts(row: usize, column: usize) -> TableFacts {
    TableFacts {
        place: Some((u32::try_from(row).unwrap_or(u32::MAX), column as u32)),
        ..TableFacts::default()
    }
}

/// Build list row `logical`'s body: the headers for `0`, else data row
/// `logical - 1`, with the cells of the columns in view.
pub(super) fn build_row(grid: &Grid, logical: usize, cx: &mut BuildCx<'_>) {
    let Some(host) = cx.parent() else {
        return;
    };
    let style = grid.style;
    let h = height(grid, logical);
    let (first, end) = grid.window.get();
    let skip = grid.starts.borrow().get(first).copied().unwrap_or(0.0);
    let line = if logical == 0 {
        style.header_line
    } else {
        style.line
    };
    let (mut strip, mut spacer) = (None, None);
    let mut cells = VecDeque::with_capacity(end - first);
    let row = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            align: Align::Stretch,
            justify: Justify::Start,
            size: Size {
                width: Length::fill(),
                height: Length::Fixed(h),
            },
            style: BoxStyle::solid(row_fill(grid, logical)),
            ..FlexStyle::default()
        },
        |cx| {
            let built = cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    align: Align::Stretch,
                    justify: Justify::Start,
                    size: Size {
                        width: Length::fill(),
                        height: Length::Fixed(h - LINE),
                    },
                    ..FlexStyle::default()
                },
                |cx| {
                    spacer = Some(
                        cx.leaf(LeafStyle {
                            size: Size::fixed(skip, 0.0),
                            style: BoxStyle::NONE,
                        })
                        .id(),
                    );
                    if logical <= grid.rows.get() {
                        for place in first..end {
                            cells.push_back(build_cell(grid, cx, logical, place));
                        }
                    }
                },
            );
            strip = Some(built.id());
            cx.leaf(LeafStyle {
                size: Size {
                    width: Length::fill(),
                    height: Length::Fixed(LINE),
                },
                style: BoxStyle::solid(line),
            });
        },
    );
    cx.semantics(row, Semantics::role(Role::Row));
    cx.table_facts(row, place_facts(logical, 0));
    if grid.select == GridSelect::Rows && logical > 0 {
        let selected = row_selected(grid, logical);
        cx.semantic_state(
            row,
            SemanticState {
                selected: Some(selected),
                ..SemanticState::default()
            },
        );
    }
    grid.built_rows.borrow_mut().insert(
        host,
        RowCells {
            logical,
            row: row.id(),
            strip: strip.expect("the strip is built"),
            spacer: spacer.expect("the spacer is built"),
            first,
            cells,
        },
    );
}

/// Build the cell of list row `logical` at place `place` on screen.
fn build_cell(grid: &Grid, cx: &mut BuildCx<'_>, logical: usize, place: usize) -> NodeId {
    if logical == 0 {
        return header_cell(grid, cx, place);
    }
    let style = grid.style;
    let model = grid.model(place);
    let (width, justify, align) = {
        let columns = grid.columns.borrow();
        let column = &columns[model];
        (column.width, column.justify(), column.align)
    };
    let row = logical - 1;
    let look = cell_look(grid, logical, place);
    let cell = cx.flex(
        FlexStyle {
            axis: Axis::Row,
            align: Align::Center,
            justify,
            padding: Inset {
                left: style.padding,
                right: style.padding,
                ..Inset::default()
            },
            size: Size::fixed(width, height(grid, logical) - LINE),
            style: BoxStyle {
                fill: look.fill,
                border: look.border,
                ..BoxStyle::NONE
            },
            ..FlexStyle::default()
        },
        |cx| {
            if let Some(build) = &grid.cell {
                build(cx, row, model);
            } else if let Some(text) = &grid.text {
                label(text(row, model))
                    .font_size(style.font_size)
                    .color(style.text)
                    .ellipsis()
                    .align(align)
                    .size(Size {
                        width: Length::fill(),
                        height: Length::Fit,
                    })
                    .build(cx);
            }
        },
    );
    cx.clip(cell, true);
    show_if_cursor(grid, cx, cell, logical, place);
    cx.semantics(cell, Semantics::role(Role::GridCell));
    cx.table_facts(cell, place_facts(logical, place));
    if grid.select != GridSelect::None {
        cx.semantic_state(
            cell,
            SemanticState {
                selected: Some(look.selected),
                ..SemanticState::default()
            },
        );
    }
    cell.id()
}

/// Build the header at place `place`: its title, its sort, and the grip
/// its column is resized by.
fn header_cell(grid: &Grid, cx: &mut BuildCx<'_>, place: usize) -> NodeId {
    let style = grid.style;
    let model = grid.model(place);
    let (title, width, align, resizable) = {
        let columns = grid.columns.borrow();
        let column = &columns[model];
        (
            column.title.clone(),
            column.width,
            column.align,
            column.resizable,
        )
    };
    let sort = grid
        .sort
        .get()
        .filter(|&(column, _)| column == model)
        .map(|(_, order)| order);
    let look = cell_look(grid, 0, place);
    let cell = cx.flex(
        FlexStyle {
            axis: Axis::Row,
            align: Align::Center,
            justify: Justify::Start,
            padding: Inset {
                left: style.padding,
                ..Inset::default()
            },
            size: Size::fixed(width, style.header_height - LINE),
            style: BoxStyle {
                border: look.border,
                ..BoxStyle::NONE
            },
            ..FlexStyle::default()
        },
        |cx| {
            label(title.clone())
                .font_size(style.font_size)
                .color(style.header_text)
                .weight(FontWeight(600))
                .ellipsis()
                .align(align)
                .size(Size {
                    width: Length::fill(),
                    height: Length::Fit,
                })
                .build(cx);
            if let Some(order) = sort {
                glyph_slot(cx, |cx| {
                    match order {
                        SortOrder::Ascending => chevron_up(cx, style.header_text),
                        SortOrder::Descending => chevron_down(cx, style.header_text),
                    };
                });
            }
            let grip = cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    align: Align::Center,
                    justify: Justify::Center,
                    size: Size {
                        width: Length::Fixed(GRIP),
                        height: Length::fill(),
                    },
                    ..FlexStyle::default()
                },
                |cx| {
                    cx.leaf(LeafStyle {
                        size: Size {
                            width: Length::Fixed(LINE),
                            height: Length::Fixed((style.header_height * 0.5).round()),
                        },
                        style: BoxStyle::solid(style.header_line),
                    });
                },
            );
            if resizable {
                cx.cursor(grip, CursorIcon::ResizeCol);
            }
        },
    );
    cx.clip(cell, true);
    show_if_cursor(grid, cx, cell, 0, place);
    cx.semantics(cell, Semantics::role(Role::ColumnHeader).with_label(title));
    cx.table_facts(
        cell,
        TableFacts {
            sort,
            ..place_facts(0, place)
        },
    );
    cell.id()
}

/// Show a cell built at the cursor as the grid's active descendant.
fn show_if_cursor(grid: &Grid, cx: &mut BuildCx<'_>, cell: Handle, logical: usize, place: usize) {
    if grid.cursor.get() == (logical, place)
        && let Some(built) = grid.built.get()
    {
        cx.active_descendant(built.viewport, cell);
    }
}

/// How a cell shows the selection and the cursor.
struct CellLook {
    fill: Rgba,
    border: Border,
    selected: bool,
}

fn selection_fill(grid: &Grid) -> Rgba {
    if grid.focused.get() {
        grid.style.selection
    } else {
        grid.style.selection_idle
    }
}

fn row_selected(grid: &Grid, logical: usize) -> bool {
    logical > 0 && grid.selection.borrow().holds_row(grid.select, logical - 1)
}

/// What shows behind list row `logical`.
fn row_fill(grid: &Grid, logical: usize) -> Rgba {
    let style = grid.style;
    if logical == 0 {
        style.header_fill
    } else if row_selected(grid, logical) {
        selection_fill(grid)
    } else if grid.hovered.get() == Some(logical) {
        style.hover
    } else if logical.is_multiple_of(2) {
        style.stripe
    } else {
        Rgba::TRANSPARENT
    }
}

fn cell_look(grid: &Grid, logical: usize, place: usize) -> CellLook {
    let selected = logical > 0
        && grid
            .selection
            .borrow()
            .holds(grid.select, (logical - 1, place));
    let fill = if selected && grid.select == GridSelect::Cells {
        selection_fill(grid)
    } else {
        Rgba::TRANSPARENT
    };
    let cursor = grid.focused.get() && grid.cursor.get() == (logical, place);
    let border = if cursor {
        Border {
            width: grid.style.cursor_width,
            color: grid.style.cursor,
        }
    } else {
        Border::NONE
    };
    CellLook {
        fill,
        border,
        selected,
    }
}

/// Tell an assistive technology the grid's rows and columns.
pub(super) fn tell_extent(grid: &Grid, store: &mut NodeStore) {
    let Some(built) = grid.built.get() else {
        return;
    };
    let rows = u32::try_from(grid.rows.get() + 1).unwrap_or(u32::MAX);
    let columns = grid.column_count() as u32;
    store.set_table_facts(
        built.viewport,
        TableFacts {
            extent: Some((rows, columns)),
            ..TableFacts::default()
        },
    );
}

/// The viewport now shows `view` across: build and free cells in the rows
/// mounted so each holds the columns in view.
pub(super) fn seen_across(
    grid: &Grid,
    s: &mut StructureCx<'_>,
    view: AcrossView,
    rows: &[(usize, NodeId)],
) {
    grid.view.set(view);
    let window = columns::window(&grid.starts.borrow(), view);
    if window == grid.window.get() {
        return;
    }
    grid.window.set(window);
    let mut scratch = Vec::new();
    for &(logical, host) in rows {
        shift_row(grid, s, host, logical, &mut scratch);
    }
    s.bindings.collect(s.store, s.states);
}

/// Bring the cells of the row hosted by `host` to the columns in view,
/// building those entering and freeing those leaving.
fn shift_row(
    grid: &Grid,
    s: &mut StructureCx<'_>,
    host: NodeId,
    logical: usize,
    scratch: &mut Vec<NodeId>,
) {
    let (first, end) = grid.window.get();
    let Some(mut rc) = take_row(grid, s.store, host, logical) else {
        return;
    };
    let len = rc.cells.len();
    if len == 0 || end <= rc.first || first >= rc.first + len {
        free_cells(s, &mut rc, scratch);
        rc.first = first;
        for place in first..end {
            let cell = build_in(grid, s, rc.strip, logical, place);
            rc.cells.push_back(cell);
        }
    } else {
        while rc.first < first {
            if let Some(cell) = rc.cells.pop_front() {
                s.store.free_tree(cell, s.effects, scratch);
            }
            rc.first += 1;
        }
        while rc.first + rc.cells.len() > end {
            if let Some(cell) = rc.cells.pop_back() {
                s.store.free_tree(cell, s.effects, scratch);
            }
        }
        for place in (first..rc.first).rev() {
            let cell = build_in(grid, s, rc.strip, logical, place);
            let before = rc.cells.front().copied();
            s.store.arena_detach(cell);
            s.store.arena_insert_before(rc.strip, cell, before);
            rc.cells.push_front(cell);
        }
        rc.first = rc.first.min(first);
        for place in rc.first + rc.cells.len()..end {
            let cell = build_in(grid, s, rc.strip, logical, place);
            rc.cells.push_back(cell);
        }
    }
    let skip = grid.starts.borrow().get(first).copied().unwrap_or(0.0);
    s.store.set_fixed_size(rc.spacer, Size::fixed(skip, 0.0));
    grid.built_rows.borrow_mut().insert(host, rc);
}

/// The record of the row hosted by `host`, if it still shows list row
/// `logical`.
fn take_row(grid: &Grid, store: &NodeStore, host: NodeId, logical: usize) -> Option<RowCells> {
    let mut rows = grid.built_rows.borrow_mut();
    let rc = rows.remove(&host)?;
    (rc.logical == logical && store.arena().is_live(rc.strip)).then_some(rc)
}

fn free_cells(s: &mut StructureCx<'_>, rc: &mut RowCells, scratch: &mut Vec<NodeId>) {
    for cell in rc.cells.drain(..) {
        s.store.free_tree(cell, s.effects, scratch);
    }
}

/// Build the cell at `place` of list row `logical` last in `strip`.
fn build_in(
    grid: &Grid,
    s: &mut StructureCx<'_>,
    strip: NodeId,
    logical: usize,
    place: usize,
) -> NodeId {
    let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, strip);
    build_cell(grid, &mut cx, logical, place)
}

/// The rows mounted, as `(logical, host)`.
fn mounted(grid: &Grid, s: &StructureCx<'_>) -> Vec<(usize, NodeId)> {
    let Some(built) = grid.built.get() else {
        return Vec::new();
    };
    s.lists
        .get(built.viewport)
        .map(|list| list.mounted_rows().collect())
        .unwrap_or_default()
}

/// Apply what the handlers asked since the last pass.
pub(super) fn apply(grid: &Grid, s: &mut StructureCx<'_>) {
    let pending = grid.pending.replace(Pending::default());
    let Some(built) = grid.built.get() else {
        return;
    };
    if pending.has(Pending::COUNT) {
        set_item_count(s.lists, built.viewport, grid.rows.get() + 1);
        tell_extent(grid, s.store);
    }
    if pending.has(Pending::WIDTHS) || pending.has(Pending::CELLS) {
        grid.place_columns();
        s.store.set_list_across(built.viewport, grid.total_width());
        grid.window
            .set(columns::window(&grid.starts.borrow(), grid.view.get()));
    }
    let rows = mounted(grid, s);
    let mut scratch = Vec::new();
    if pending.has(Pending::CELLS) || pending.has(Pending::HEADERS) {
        let all = pending.has(Pending::CELLS);
        for &(logical, host) in &rows {
            if !all && logical != 0 {
                continue;
            }
            if let Some(mut rc) = take_row(grid, s.store, host, logical) {
                free_cells(s, &mut rc, &mut scratch);
                grid.built_rows.borrow_mut().insert(host, rc);
                shift_row(grid, s, host, logical, &mut scratch);
            }
        }
        s.bindings.collect(s.store, s.states);
    } else if pending.has(Pending::WIDTHS) {
        for &(logical, host) in &rows {
            resize_row(grid, s, host, logical);
            shift_row(grid, s, host, logical, &mut scratch);
        }
        s.bindings.collect(s.store, s.states);
    }
    if pending.has(Pending::LOOKS) {
        for &(logical, host) in &rows {
            restyle_row(grid, s.store, host, logical);
        }
    }
    if pending.has(Pending::REVEAL) {
        reveal(grid, s);
    }
    show_active(grid, s.store);
}

/// Resize the cells built in the row hosted by `host` to their columns'
/// widths.
fn resize_row(grid: &Grid, s: &mut StructureCx<'_>, host: NodeId, logical: usize) {
    let rows = grid.built_rows.borrow();
    let Some(rc) = rows.get(&host).filter(|rc| rc.logical == logical) else {
        return;
    };
    let columns = grid.columns.borrow();
    let order = grid.order.borrow();
    let h = height(grid, logical) - LINE;
    for (k, &cell) in rc.cells.iter().enumerate() {
        let Some(&model) = order.get(rc.first + k) else {
            continue;
        };
        s.store
            .set_fixed_size(cell, Size::fixed(columns[model].width, h));
    }
    s.store.mark_dirty(rc.strip, DirtyClass::LAYOUT);
}

/// Restyle the row hosted by `host` and its cells for the selection, the
/// cursor and the hover.
fn restyle_row(grid: &Grid, store: &mut NodeStore, host: NodeId, logical: usize) {
    let rows = grid.built_rows.borrow();
    let Some(rc) = rows.get(&host).filter(|rc| rc.logical == logical) else {
        return;
    };
    store.set_fill(rc.row, row_fill(grid, logical));
    if grid.select == GridSelect::Rows && logical > 0 {
        let selected = row_selected(grid, logical);
        let state = store.semantic_state(rc.row).unwrap_or_default();
        if state.selected != Some(selected) {
            store.set_semantic_state(
                rc.row,
                SemanticState {
                    selected: Some(selected),
                    ..state
                },
            );
        }
    }
    for (k, &cell) in rc.cells.iter().enumerate() {
        let look = cell_look(grid, logical, rc.first + k);
        if logical > 0 {
            store.set_fill(cell, look.fill);
        }
        store.set_border(cell, look.border);
        if logical > 0 && grid.select != GridSelect::None {
            let state = store.semantic_state(cell).unwrap_or_default();
            if state.selected != Some(look.selected) {
                store.set_semantic_state(
                    cell,
                    SemanticState {
                        selected: Some(look.selected),
                        ..state
                    },
                );
            }
        }
    }
}

/// The cell built at the cursor, if it is built.
fn cursor_cell(grid: &Grid, store: &NodeStore) -> Option<NodeId> {
    let (logical, place) = grid.cursor.get();
    let rows = grid.built_rows.borrow();
    rows.values()
        .find(|rc| rc.logical == logical && store.arena().is_live(rc.strip))
        .filter(|rc| (rc.first..rc.first + rc.cells.len()).contains(&place))
        .map(|rc| rc.cells[place - rc.first])
        .filter(|&cell| store.arena().is_live(cell))
}

/// Show the cursor's cell as the grid's active descendant.
pub(super) fn show_active(grid: &Grid, store: &mut NodeStore) {
    let Some(built) = grid.built.get() else {
        return;
    };
    let active = cursor_cell(grid, store);
    if store.active_descendant(built.viewport) != active {
        store.set_active_descendant(built.viewport, active);
    }
}

/// Scroll the least that brings the cursor's cell into view, below the
/// headers: the list shows its row, and the grid its column.
fn reveal(grid: &Grid, s: &mut StructureCx<'_>) {
    let Some(built) = grid.built.get() else {
        return;
    };
    let viewport = built.viewport;
    let (logical, place) = grid.cursor.get();
    if logical > 0
        && let Some(list) = s.lists.get_mut(viewport)
    {
        list.show_index(logical);
    }
    let store = &mut *s.store;
    let range = store.scroll_range(viewport, Axis::Row);
    let span = {
        let starts = grid.starts.borrow();
        starts
            .get(place)
            .copied()
            .zip(starts.get(place + 1).copied())
    };
    let Some((start, end)) = span.filter(|_| range > 0.0) else {
        return;
    };
    let view = store.content(viewport).x - range;
    let now = store.scroll(viewport);
    let rtl = store.layout_direction(viewport) == viso_ui::adaptive::LayoutDirection::Rtl;
    let from = if rtl { -now.x } else { now.x };
    let mut distance = from;
    if start < from {
        distance = start;
    } else if end > from + view {
        distance = (end - view).min(start);
    }
    if distance != from {
        let x = if rtl { -distance } else { distance };
        store.set_scroll(viewport, Vec2::new(x, now.y));
    }
}
