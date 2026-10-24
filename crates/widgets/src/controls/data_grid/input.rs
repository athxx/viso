//! What a grid does with the pointer and the keys: hit testing by its
//! geometry, the header gestures (sort, move, resize), the selection and
//! the cursor.

use std::cell::RefCell;

use viso_ui::{
    DispatchPhase, EventCx, FocusChange, Key, KeyEvent, PointerButtons, PointerEvent, PointerPhase,
    Rect, primary_is_logo,
};

use super::cells::GRIP;
use super::{Gesture, Grid, GridSelect, Pending, columns, restore};

/// How far a header is dragged before its column follows.
const SLOP: f32 = 4.0;
/// How much Alt with Left/Right resizes a column by.
const NUDGE: f32 = 10.0;

/// Run the handler in `slot` with `f`, putting it back after.
fn with<H>(slot: &RefCell<Option<H>>, f: impl FnOnce(&mut H)) {
    let handler = slot.borrow_mut().take();
    if let Some(mut handler) = handler {
        f(&mut handler);
        restore(slot, handler);
    }
}

/// Where a pointer is over the grid.
struct Spot {
    /// Over the headers.
    header: bool,
    /// The list row under it, past the last row or before the first data
    /// row as it is: clamped by those that need a cell.
    logical: isize,
    /// How far from the grid's start edge it is, across.
    distance: f32,
}

fn locate(grid: &Grid, p: &PointerEvent, viewport: Rect, canvas: Rect, rtl: bool) -> Spot {
    let style = grid.style;
    let header = p.y >= viewport.y && p.y - viewport.y < style.header_height;
    let below = p.y - canvas.y - style.header_height;
    let logical = if header {
        0
    } else {
        (below / style.row_height).floor() as isize + 1
    };
    let distance = if rtl {
        canvas.x + canvas.w - p.x
    } else {
        p.x - canvas.x
    };
    Spot {
        header,
        logical,
        distance,
    }
}

/// The place under `distance`, clamped to the columns.
fn place_near(grid: &Grid, distance: f32) -> Option<usize> {
    let starts = grid.starts.borrow();
    let count = starts.len().checked_sub(1).filter(|&n| n > 0)?;
    Some(columns::at(&starts, distance).unwrap_or(if distance < 0.0 { 0 } else { count - 1 }))
}

/// The place whose header's resize grip is under `distance`.
fn grip_at(grid: &Grid, distance: f32) -> Option<usize> {
    let starts = grid.starts.borrow();
    let place = columns::at(&starts, distance)?;
    let resizable = grid.columns.borrow()[grid.order.borrow()[place]].resizable;
    (resizable && starts[place + 1] - distance <= GRIP).then_some(place)
}

fn primary(m: viso_ui::Modifiers) -> bool {
    if primary_is_logo() { m.logo } else { m.control }
}

pub(super) fn pointer(grid: &Grid, ev: &mut EventCx<'_>) {
    if ev.phase() == DispatchPhase::Capture {
        return;
    }
    let (Some(p), Some(built)) = (ev.pointer().copied(), grid.built.get()) else {
        return;
    };
    let (Some(viewport), Some(canvas)) = (ev.rect_of(built.viewport), ev.rect_of(built.canvas))
    else {
        return;
    };
    let spot = locate(grid, &p, viewport, canvas, ev.reads_rtl());
    let rows = grid.rows.get() as isize;
    let direct = ev.direct();
    match p.phase {
        PointerPhase::Down if p.buttons.contains(PointerButtons::PRIMARY) => {
            ev.request_focus(built.viewport);
            down(grid, ev, &p, &spot, rows, direct);
        }
        PointerPhase::Move => match grid.gesture.get() {
            Gesture::None => {
                let over = (!direct && !spot.header && (1..=rows).contains(&spot.logical))
                    .then_some(spot.logical as usize);
                hover(grid, ev, over);
            }
            Gesture::Resize { place, from, width } => {
                let model = grid.model(place);
                let wide = grid.columns.borrow()[model].clamp(width + spot.distance - from);
                if wide != grid.columns.borrow()[model].width {
                    grid.columns.borrow_mut()[model].width = wide;
                    grid.place_columns();
                    grid.ask(ev, Pending::WIDTHS);
                }
            }
            Gesture::Header { place, from, moved } => {
                if direct {
                    if !ev.still() {
                        grid.gesture.set(Gesture::None);
                    }
                    return;
                }
                let moved = moved || (spot.distance - from).abs() > SLOP;
                let mut place = place;
                if moved
                    && let Some(to) = place_near(grid, spot.distance)
                    && to != place
                {
                    move_column(grid, place, to);
                    place = to;
                    grid.ask(ev, Pending::CELLS | Pending::LOOKS);
                }
                grid.gesture.set(Gesture::Header { place, from, moved });
            }
            Gesture::Select => {
                if rows == 0 {
                    return;
                }
                let logical = spot.logical.clamp(1, rows) as usize;
                if let Some(place) = place_near(grid, spot.distance) {
                    go(grid, ev, (logical, place), true);
                }
            }
            Gesture::Tap { .. } => {
                if !ev.still() {
                    grid.gesture.set(Gesture::None);
                }
            }
        },
        PointerPhase::Up => {
            up(grid, ev);
        }
        PointerPhase::Leave if grid.gesture.get() == Gesture::None => hover(grid, ev, None),
        _ => {}
    }
}

fn down(
    grid: &Grid,
    ev: &mut EventCx<'_>,
    p: &PointerEvent,
    spot: &Spot,
    rows: isize,
    direct: bool,
) {
    let Some(built) = grid.built.get() else {
        return;
    };
    if spot.header {
        if !direct && let Some(place) = grip_at(grid, spot.distance) {
            let width = grid.columns.borrow()[grid.model(place)].width;
            grid.gesture.set(Gesture::Resize {
                place,
                from: spot.distance,
                width,
            });
            ev.capture_pointer(built.viewport);
            return;
        }
        if let Some(place) = columns::at(&grid.starts.borrow(), spot.distance) {
            grid.gesture.set(Gesture::Header {
                place,
                from: spot.distance,
                moved: false,
            });
            if !direct {
                ev.capture_pointer(built.viewport);
            }
        }
        return;
    }
    if !(1..=rows).contains(&spot.logical) {
        return;
    }
    let Some(place) = columns::at(&grid.starts.borrow(), spot.distance) else {
        return;
    };
    let at = (spot.logical as usize, place);
    let (extend, add) = (p.modifiers.shift, primary(p.modifiers));
    if direct {
        grid.gesture.set(Gesture::Tap { at, extend, add });
        return;
    }
    if ev.clicks() >= 2 && !extend && !add {
        go(grid, ev, at, false);
        activate(grid, ev);
        return;
    }
    pick(grid, ev, at, extend, add);
    grid.gesture.set(Gesture::Select);
    ev.capture_pointer(built.viewport);
}

fn up(grid: &Grid, ev: &mut EventCx<'_>) {
    let gesture = grid.gesture.replace(Gesture::None);
    match gesture {
        Gesture::None => return,
        Gesture::Resize { place, .. } => {
            let model = grid.model(place);
            let width = grid.columns.borrow()[model].width;
            with(&grid.on_resize, |h| h(ev, model, width));
        }
        Gesture::Header { place, moved, .. } => {
            if moved {
                let order = grid.order.borrow().clone();
                with(&grid.on_reorder, |h| h(ev, &order));
            } else if !ev.direct() || ev.still() {
                grid.cursor.set((0, place));
                sort_by(grid, ev, place);
                grid.ask(ev, Pending::LOOKS);
            }
        }
        Gesture::Select => {}
        Gesture::Tap { at, extend, add } => {
            if ev.still() {
                pick(grid, ev, at, extend, add);
            }
        }
    }
    ev.release_pointer();
}

fn hover(grid: &Grid, ev: &mut EventCx<'_>, over: Option<usize>) {
    if grid.hovered.get() != over {
        grid.hovered.set(over);
        grid.ask(ev, Pending::LOOKS);
    }
}

/// Select at cell `at` (a list row past the headers and a place) as a
/// press with Shift (`extend`) or the primary modifier (`add`) does, and
/// put the cursor there.
fn pick(grid: &Grid, ev: &mut EventCx<'_>, at: (usize, usize), extend: bool, add: bool) {
    if at.0 == 0 {
        return;
    }
    let before = grid.selection.borrow().clone();
    let spot = (at.0 - 1, at.1);
    let from = grid.cursor.get();
    {
        let mut selection = grid.selection.borrow_mut();
        match grid.select {
            GridSelect::None => {}
            _ if extend => selection.extend((from.0.max(1) - 1, from.1), spot),
            GridSelect::Rows if add && selection.holds_row(GridSelect::Rows, spot.0) => {
                selection.drop_row(spot.0);
            }
            _ if add => selection.add(spot),
            _ => selection.only(spot),
        }
    }
    grid.cursor.set(at);
    grid.ask(ev, Pending::LOOKS | Pending::REVEAL);
    if *grid.selection.borrow() != before {
        grid.tell_selection(ev);
    }
}

/// Move the cursor to `to`, the selection following it or, with `extend`,
/// stretching to it.
fn go(grid: &Grid, ev: &mut EventCx<'_>, to: (usize, usize), extend: bool) {
    let from = grid.cursor.get();
    if to.0 == 0 || grid.select == GridSelect::None {
        grid.cursor.set(to);
        grid.ask(ev, Pending::LOOKS | Pending::REVEAL);
        return;
    }
    if to == from && extend {
        return;
    }
    pick(grid, ev, to, extend && from.0 > 0, false);
}

/// Sort by the column at place `place`: ascending, again the other way.
fn sort_by(grid: &Grid, ev: &mut EventCx<'_>, place: usize) {
    let model = grid.model(place);
    if !grid.columns.borrow()[model].sortable {
        return;
    }
    let order = match grid.sort.get() {
        Some((column, order)) if column == model => order.reversed(),
        _ => viso_ui::SortOrder::Ascending,
    };
    grid.sort.set(Some((model, order)));
    with(&grid.on_sort, |h| h(ev, model, order));
    grid.ask(ev, Pending::CELLS);
}

/// Move the column at place `from` to place `to`, the cursor keeping to the
/// column it was on.
fn move_column(grid: &Grid, from: usize, to: usize) {
    let (row, place) = grid.cursor.get();
    let on = grid.model(place);
    {
        let mut order = grid.order.borrow_mut();
        let model = order.remove(from);
        order.insert(to, model);
    }
    let place = grid
        .order
        .borrow()
        .iter()
        .position(|&m| m == on)
        .unwrap_or(place);
    grid.cursor.set((row, place));
    grid.place_columns();
}

/// Activate the cursor's cell.
fn activate(grid: &Grid, ev: &mut EventCx<'_>) {
    let (row, place) = grid.cursor.get();
    if row == 0 {
        return;
    }
    let model = grid.model(place);
    with(&grid.on_activate, |h| h(ev, row - 1, model));
}

/// The selection, or the cursor's cell, as tab-separated text.
fn copied(grid: &Grid) -> Option<String> {
    let text = grid.text.as_ref()?;
    let count = grid.column_count();
    let selection = grid.selection.borrow();
    let ranges = selection.ranges(grid.select, count);
    let order = grid.order.borrow();
    let mut out = String::new();
    let mut line = |row: usize, places: std::ops::RangeInclusive<usize>| {
        if !out.is_empty() {
            out.push('\n');
        }
        for (k, place) in places.enumerate() {
            if k > 0 {
                out.push('\t');
            }
            out.push_str(&text(row, order[place]));
        }
    };
    match (grid.select, ranges.last()) {
        (GridSelect::Rows, Some(_)) => {
            for row in selection.told(grid.select, &order).rows() {
                line(row, 0..=count - 1);
            }
        }
        (GridSelect::Cells, Some(range)) => {
            for row in range.rows.0..=range.rows.1 {
                line(row, range.columns.0..=range.columns.1);
            }
        }
        _ => {
            let (row, place) = grid.cursor.get();
            line(row.checked_sub(1)?, place..=place);
        }
    }
    Some(out)
}

pub(super) fn key(grid: &Grid, ev: &mut EventCx<'_>) {
    if let Some(change) = ev.focus_change() {
        let focused = change == FocusChange::Gained;
        if grid.focused.get() != focused {
            grid.focused.set(focused);
            grid.ask(ev, Pending::LOOKS);
        }
        return;
    }
    if ev.phase() == DispatchPhase::Capture {
        return;
    }
    let Some(k) = ev.key().copied().filter(|k| k.pressed) else {
        return;
    };
    if press(grid, ev, k) {
        ev.stop_propagation();
    }
}

/// Take key `k`; whether it was the grid's.
fn press(grid: &Grid, ev: &mut EventCx<'_>, k: KeyEvent) -> bool {
    let count = grid.column_count();
    if count == 0 {
        return false;
    }
    let m = k.modifiers;
    let primary = primary(m);
    let rows = grid.rows.get();
    let (row, place) = grid.cursor.get();
    let last = count - 1;
    // Left and Right follow the screen: read right to left, Left goes on.
    let toward = |key: Key| match (key, ev.reads_rtl()) {
        (Key::Right, false) | (Key::Left, true) => 1,
        _ => -1,
    };
    let first_row = usize::from(rows > 0);
    match k.key {
        Key::Left | Key::Right if row == 0 && primary && m.shift => {
            let to = place.saturating_add_signed(toward(k.key)).min(last);
            if to != place {
                move_column(grid, place, to);
                grid.ask(ev, Pending::CELLS | Pending::LOOKS | Pending::REVEAL);
                let order = grid.order.borrow().clone();
                with(&grid.on_reorder, |h| h(ev, &order));
            }
        }
        Key::Left | Key::Right if row == 0 && m.alt => {
            let model = grid.model(place);
            let width = {
                let mut columns = grid.columns.borrow_mut();
                let column = &mut columns[model];
                if !column.resizable {
                    return true;
                }
                column.width = column.clamp(column.width + NUDGE * toward(k.key) as f32);
                column.width
            };
            grid.place_columns();
            grid.ask(ev, Pending::WIDTHS | Pending::REVEAL);
            with(&grid.on_resize, |h| h(ev, model, width));
        }
        Key::Enter | Key::Space if row == 0 => {
            if !k.repeat {
                sort_by(grid, ev, place);
            }
        }
        Key::Left | Key::Right => {
            let to = if primary {
                if toward(k.key) > 0 { last } else { 0 }
            } else {
                place.saturating_add_signed(toward(k.key)).min(last)
            };
            go(grid, ev, (row, to), m.shift);
        }
        Key::Up | Key::Down => {
            let to = match (k.key, primary) {
                (Key::Up, true) => first_row,
                (Key::Down, true) => rows,
                (Key::Up, false) => row.saturating_sub(1),
                _ => (row + 1).min(rows),
            };
            go(grid, ev, (to, place), m.shift);
        }
        Key::Home | Key::End => {
            let end = k.key == Key::End;
            let to_row = match (primary, end) {
                (true, false) => first_row,
                (true, true) => rows,
                _ => row,
            };
            go(grid, ev, (to_row, if end { last } else { 0 }), m.shift);
        }
        Key::PageUp | Key::PageDown => {
            let style = grid.style;
            let shown = grid
                .built
                .get()
                .and_then(|b| ev.rect_of(b.viewport))
                .map_or(0.0, |r| r.h);
            let page = (((shown - style.header_height) / style.row_height).floor() as usize).max(1);
            let to = if k.key == Key::PageDown {
                (row + page).min(rows)
            } else {
                row.saturating_sub(page).max(first_row)
            };
            go(grid, ev, (to, place), m.shift);
        }
        Key::Space if grid.select != GridSelect::None && !k.repeat => {
            pick(grid, ev, (row, place), false, true);
        }
        Key::A if primary => {
            if grid.select != GridSelect::None {
                grid.selection.borrow_mut().all(rows, count);
                grid.ask(ev, Pending::LOOKS);
                grid.tell_selection(ev);
            }
        }
        Key::C if primary => {
            if let Some(text) = copied(grid) {
                ev.copy_text(text);
            }
        }
        Key::Enter | Key::F2 if !k.repeat => activate(grid, ev),
        Key::Escape => {
            let at = (row.max(1) - 1, place);
            let only = grid.selection.borrow().is_only(at);
            if row > 0 && grid.select != GridSelect::None && !only {
                grid.selection.borrow_mut().only(at);
                grid.ask(ev, Pending::LOOKS);
                grid.tell_selection(ev);
            } else {
                return false;
            }
        }
        _ => return false,
    }
    true
}
