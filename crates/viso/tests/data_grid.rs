//! Data grids driven through the facade: only the rows and the columns in
//! view are built, scrolling across builds and frees columns, a press
//! selects (Shift extends, the primary modifier adds), the keyboard moves
//! the cursor, a header sorts, its edge resizes its column and a drag moves
//! it, a copy answers with tab-separated text, and the grid tells an
//! assistive technology its extent, each cell's place and the sort.
#![cfg(feature = "headless")]

use std::cell::{Cell, RefCell};
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    ClipboardReply, KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, RawScroll, ScrollPhase, WindowId,
};
use viso::prelude::*;
use viso::ui::adaptive::LayoutDirection;
use viso::ui::{Axis, Component, FlexStyle, NodeId, NodeStore, Role, Sizing, SortOrder};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
/// Ten columns 120 wide in a grid 400 wide and 300 tall, at the origin.
const W: f32 = 120.0;
const ROW: f32 = 28.0;
const HEAD: f32 = 32.0;

thread_local! {
    static HEARD: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static CELLS: Cell<bool> = const { Cell::new(false) };
    static RTL: Cell<bool> = const { Cell::new(false) };
}

fn heard() -> Vec<String> {
    HEARD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

fn hear(what: String) {
    HEARD.with(|h| h.borrow_mut().push(what));
}

struct GridApp;

impl Application for GridApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        GridApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let columns = (0..10)
            .map(|c| {
                let column = grid_column(format!("C{c}")).width(W);
                if c < 2 { column.sortable() } else { column }
            })
            .collect();
        let select = if CELLS.with(Cell::get) {
            GridSelect::Cells
        } else {
            GridSelect::Rows
        };
        // A root viewport fills the window: the grid sits in a column.
        let direction = RTL.with(Cell::get).then_some(LayoutDirection::Rtl);
        cx.adaptive_scope(
            None,
            direction,
            FlexStyle {
                size: Sizing::fill(),
                axis: Axis::Column,
                ..FlexStyle::default()
            },
            |cx| {
                data_grid("Orders", 1000, columns)
                    .text(|row, column| format!("r{row}c{column}"))
                    .select(select)
                    .size(Sizing::fixed(400.0, 300.0))
                    .on_select(|_, s| hear(format!("select {:?}", s.rows())))
                    .on_sort(|_, column, order| hear(format!("sort {column} {order:?}")))
                    .on_activate(|_, row, column| hear(format!("activate {row} {column}")))
                    .on_resize(|_, column, width| hear(format!("resize {column} {width}")))
                    .on_reorder(|_, order| hear(format!("reorder {:?}", &order[..3])))
                    .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f32, y: f32, buttons: RawButtons, mods: RawModifiers, phase: RawPhase) -> RawEvent {
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        f64::from(x),
        f64::from(y),
        buttons,
        mods,
        phase,
    ))
}

fn click_with(at: (f32, f32), mods: RawModifiers) -> Vec<RawEvent> {
    vec![
        mouse(at.0, at.1, RawButtons::NONE, mods, RawPhase::Moved),
        mouse(at.0, at.1, RawButtons::PRIMARY, mods, RawPhase::Down),
        mouse(at.0, at.1, RawButtons::PRIMARY, mods, RawPhase::Up),
        redraw(),
        redraw(),
    ]
}

fn click(at: (f32, f32)) -> Vec<RawEvent> {
    click_with(at, RawModifiers::default())
}

fn drag(from: (f32, f32), to: (f32, f32)) -> Vec<RawEvent> {
    let none = RawModifiers::default();
    let mut out = vec![
        mouse(from.0, from.1, RawButtons::NONE, none, RawPhase::Moved),
        mouse(from.0, from.1, RawButtons::PRIMARY, none, RawPhase::Down),
        redraw(),
    ];
    for k in 1..=4 {
        let f = k as f32 / 4.0;
        let x = from.0 + (to.0 - from.0) * f;
        let y = from.1 + (to.1 - from.1) * f;
        out.push(mouse(x, y, RawButtons::PRIMARY, none, RawPhase::Moved));
        out.push(redraw());
    }
    out.push(mouse(to.0, to.1, RawButtons::PRIMARY, none, RawPhase::Up));
    out.extend([redraw(), redraw()]);
    out
}

fn key_with(code: Key, modifiers: RawModifiers) -> Vec<RawEvent> {
    let raw = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers,
        })
    };
    vec![raw(true), raw(false), redraw(), redraw()]
}

fn key(code: Key) -> Vec<RawEvent> {
    key_with(code, RawModifiers::default())
}

fn shift() -> RawModifiers {
    RawModifiers {
        shift: true,
        ..RawModifiers::default()
    }
}

fn primary() -> RawModifiers {
    let apple = viso::ui::primary_is_logo();
    RawModifiers {
        logo: apple,
        control: !apple,
        ..RawModifiers::default()
    }
}

fn wheel(dx: f32, dy: f32) -> Vec<RawEvent> {
    vec![
        RawEvent::Scroll(RawScroll {
            window: WIN,
            x: 200.0,
            y: 150.0,
            delta_x: f64::from(dx),
            delta_y: f64::from(dy),
            modifiers: RawModifiers::default(),
            phase: ScrollPhase::None,
        }),
        redraw(),
        redraw(),
        redraw(),
    ]
}

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<GridApp> {
    heard();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<GridApp>(script, STEP)
}

/// Every node under `root` for which `keep` holds, in tree order.
fn find(store: &NodeStore, root: NodeId, keep: &dyn Fn(NodeId) -> bool) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if keep(id) {
            out.push(id);
        }
        let kids: Vec<NodeId> = store.arena().children(id).collect();
        stack.extend(kids.into_iter().rev());
    }
    out
}

fn role(store: &NodeStore, id: NodeId) -> Option<Role> {
    store.semantics(id).map(|s| s.role)
}

fn grid(app: &DrivenApp<GridApp>) -> NodeId {
    let store = app.store();
    find(store, app.root().unwrap(), &|id| {
        role(store, id) == Some(Role::Grid)
    })[0]
}

/// The cells built, by `(list row, place)`.
fn cells(app: &DrivenApp<GridApp>) -> Vec<((u32, u32), NodeId)> {
    let store = app.store();
    let mut out: Vec<_> = find(store, grid(app), &|id| {
        matches!(role(store, id), Some(Role::GridCell | Role::ColumnHeader))
    })
    .into_iter()
    .map(|id| (store.table_facts(id).place.unwrap(), id))
    .collect();
    out.sort_unstable_by_key(|&(at, _)| at);
    out
}

/// The places built in list row `row`.
fn places(app: &DrivenApp<GridApp>, row: u32) -> Vec<u32> {
    cells(app)
        .into_iter()
        .filter(|&((r, _), _)| r == row)
        .map(|((_, p), _)| p)
        .collect()
}

fn cell(app: &DrivenApp<GridApp>, at: (u32, u32)) -> NodeId {
    cells(app)
        .into_iter()
        .find(|&(a, _)| a == at)
        .map(|(_, id)| id)
        .unwrap_or_else(|| panic!("cell {at:?} is built"))
}

/// Where data row `row`'s cell at place `place` shows, unscrolled.
fn at(row: usize, place: usize) -> (f32, f32) {
    (
        place as f32 * W + W / 2.0,
        HEAD + row as f32 * ROW + ROW / 2.0,
    )
}

fn header(place: usize) -> (f32, f32) {
    (place as f32 * W + W / 2.0, HEAD / 2.0)
}

fn selected_rows(app: &DrivenApp<GridApp>) -> Vec<u32> {
    let store = app.store();
    let mut rows: Vec<u32> = find(store, grid(app), &|id| {
        role(store, id) == Some(Role::Row)
            && store.semantic_state(id).and_then(|s| s.selected) == Some(true)
    })
    .into_iter()
    .map(|id| store.table_facts(id).place.unwrap().0)
    .collect();
    rows.sort_unstable();
    rows
}

#[test]
fn it_builds_only_the_rows_and_columns_in_view() {
    let app = run(&[]);
    let store = app.store();
    let grid = grid(&app);
    assert_eq!(store.table_facts(grid).extent, Some((1001, 10)));
    let rows: Vec<u32> = cells(&app).iter().map(|((r, _), _)| *r).collect();
    let mounted = rows.iter().copied().max().unwrap();
    assert!(mounted < 30, "a screenful of rows: {mounted}");
    // 400 across shows four columns, and one more past them.
    assert_eq!(places(&app, 0), [0, 1, 2, 3, 4]);
    assert_eq!(places(&app, 1), [0, 1, 2, 3, 4]);
    let head = cell(&app, (0, 0));
    assert_eq!(role(store, head), Some(Role::ColumnHeader));
    assert_eq!(store.semantics(head).unwrap().label.as_deref(), Some("C0"));
    let r = store.world(cell(&app, (2, 1)));
    assert_eq!((r.x, r.y, r.w), (W, HEAD + ROW, W));
}

#[test]
fn scrolling_across_builds_the_columns_coming_into_view_and_frees_the_rest() {
    let app = run(&[wheel(500.0, 0.0)]);
    assert_eq!(app.store().scroll(grid(&app)).x, 500.0);
    // 500..900 shows columns 4 to 7; one each side is built too.
    assert_eq!(places(&app, 1), [3, 4, 5, 6, 7, 8]);
    assert_eq!(places(&app, 0), [3, 4, 5, 6, 7, 8], "the headers too");
    let r = app.store().world(cell(&app, (1, 4)));
    assert_eq!(r.x, 4.0 * W - 500.0);
    // Scrolling down keeps the headers on top and the columns.
    let app = run(&[wheel(500.0, 0.0), wheel(0.0, 2000.0)]);
    assert_eq!(app.store().world(cell(&app, (0, 4))).y, 0.0, "pinned");
    let low = cells(&app).iter().map(|((r, _), _)| *r).max().unwrap();
    assert!(low > 60, "the rows further down: {low}");
    assert_eq!(places(&app, low), [3, 4, 5, 6, 7, 8]);
}

#[test]
fn a_press_selects_a_row_shift_extends_and_the_primary_modifier_adds() {
    let app = run(&[click(at(1, 0))]);
    assert_eq!(heard(), ["select [1]"]);
    assert_eq!(selected_rows(&app), [2]);
    let g = grid(&app);
    assert_eq!(app.store().focused(), Some(g));
    assert_eq!(app.store().active_descendant(g), Some(cell(&app, (2, 0))));

    let app = run(&[click(at(1, 0)), click_with(at(3, 2), shift())]);
    assert_eq!(heard(), ["select [1]", "select [1, 2, 3]"]);
    assert_eq!(selected_rows(&app), [2, 3, 4]);

    let app = run(&[
        click(at(1, 0)),
        click_with(at(3, 2), shift()),
        click_with(at(2, 0), primary()),
        click_with(at(6, 0), primary()),
    ]);
    assert_eq!(selected_rows(&app), [2, 4, 7], "dropped one, added one");
}

#[test]
fn the_keyboard_moves_the_cursor_and_shift_extends() {
    let app = run(&[
        click(at(0, 0)),
        key(Key::Down),
        key(Key::Right),
        key_with(Key::Down, shift()),
    ]);
    assert_eq!(
        heard(),
        ["select [0]", "select [1]", "select [1]", "select [1, 2]"]
    );
    assert_eq!(
        app.store().active_descendant(grid(&app)),
        Some(cell(&app, (3, 1)))
    );
    // End goes to the last column, scrolling it into view; Control or
    // Command with Down to the last row.
    let app = run(&[
        click(at(0, 0)),
        key(Key::End),
        key_with(Key::Down, primary()),
    ]);
    let g = grid(&app);
    let scroll = app.store().scroll(g);
    assert_eq!(scroll.x, 10.0 * W - 400.0);
    assert_eq!(
        scroll.y,
        app.store().scroll_range(g, Axis::Column),
        "at the bottom"
    );
    assert_eq!(
        app.store().active_descendant(g),
        Some(cell(&app, (1000, 9)))
    );
    // Page Down moves by the rows shown.
    let app = run(&[click(at(0, 0)), key(Key::PageDown)]);
    assert_eq!(
        app.store().active_descendant(grid(&app)),
        Some(cell(&app, (10, 0))),
        "(300 - 32) / 28 rows on"
    );
    let app = run(&[click(at(0, 0)), key_with(Key::A, primary())]);
    assert_eq!(
        heard().last().map(|s| s.starts_with("select [0, 1, 2")),
        Some(true)
    );
    assert!(selected_rows(&app).len() > 9);
}

#[test]
fn enter_and_a_double_press_activate_a_cell() {
    run(&[click(at(2, 1)), key(Key::Enter)]);
    assert_eq!(heard(), ["select [2]", "activate 2 1"]);
    run(&[click(at(4, 2)), click(at(4, 2))]);
    assert_eq!(heard(), ["select [4]", "activate 4 2"]);
}

#[test]
fn a_header_sorts_and_again_the_other_way() {
    let app = run(&[click(header(1))]);
    assert_eq!(heard(), ["sort 1 Ascending"]);
    let facts = app.store().table_facts(cell(&app, (0, 1)));
    assert_eq!(facts.sort, Some(SortOrder::Ascending));
    assert_eq!(app.store().table_facts(cell(&app, (0, 0))).sort, None);
    let app = run(&[click(header(1)), click(header(1))]);
    assert_eq!(heard(), ["sort 1 Ascending", "sort 1 Descending"]);
    assert_eq!(
        app.store().table_facts(cell(&app, (0, 1))).sort,
        Some(SortOrder::Descending)
    );
    run(&[click(header(4))]);
    assert!(heard().is_empty(), "not a sortable column");
    // The keyboard sorts on the headers.
    run(&[click(at(0, 0)), key(Key::Up), key(Key::Enter)]);
    assert_eq!(heard(), ["select [0]", "sort 0 Ascending"]);
}

#[test]
fn a_header_edge_resizes_its_column_and_a_drag_moves_it() {
    let app = run(&[drag((W - 2.0, HEAD / 2.0), (W + 48.0, HEAD / 2.0))]);
    assert_eq!(heard(), ["resize 0 170"]);
    let r = app.store().world(cell(&app, (1, 1)));
    assert_eq!(r.x, 170.0, "the next column moved over");
    assert_eq!(app.store().world(cell(&app, (1, 0))).w, 170.0);

    let app = run(&[drag(header(0), (W * 1.5, HEAD / 2.0))]);
    assert_eq!(heard(), ["reorder [1, 0, 2]"]);
    let store = app.store();
    assert_eq!(
        store
            .semantics(cell(&app, (0, 0)))
            .unwrap()
            .label
            .as_deref(),
        Some("C1")
    );
    let text = |at| {
        let cell = cell(&app, at);
        find(store, cell, &|id| role(store, id) == Some(Role::Label))
            .first()
            .and_then(|&id| store.semantics(id).and_then(|s| s.label.clone()))
    };
    assert_eq!(text((1, 0)).as_deref(), Some("r0c1"));
    assert_eq!(text((1, 1)).as_deref(), Some("r0c0"));
}

#[test]
fn a_copy_answers_with_the_selection_as_tab_separated_text() {
    let reply = ClipboardReply::new();
    let mut script = vec![click(at(1, 0)), click_with(at(2, 0), shift())];
    script.push(vec![
        RawEvent::CopyRequested {
            window: WIN,
            cut: false,
            reply: reply.clone(),
        },
        redraw(),
    ]);
    run(&script);
    let text = reply.take().and_then(|d| d.text).expect("the grid copied");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("r1c0\tr1c1\t"));
    assert!(lines[1].ends_with("\tr2c9"));
}

#[test]
fn in_cell_mode_a_drag_selects_a_rectangle() {
    CELLS.with(|c| c.set(true));
    let reply = ClipboardReply::new();
    let mut script = vec![drag(at(1, 1), at(2, 2))];
    script.push(vec![
        RawEvent::CopyRequested {
            window: WIN,
            cut: false,
            reply: reply.clone(),
        },
        redraw(),
    ]);
    let app = run(&script);
    CELLS.with(|c| c.set(false));
    assert_eq!(
        reply.take().and_then(|d| d.text).as_deref(),
        Some("r1c1\tr1c2\nr2c1\tr2c2")
    );
    let store = app.store();
    let selected = |at| {
        store
            .semantic_state(cell(&app, at))
            .and_then(|s| s.selected)
    };
    assert_eq!(selected((2, 1)), Some(true));
    assert_eq!(selected((3, 2)), Some(true));
    assert_eq!(selected((2, 0)), Some(false));
    assert!(selected_rows(&app).is_empty(), "no row is selected whole");
}

#[test]
fn read_right_to_left_the_columns_run_from_the_right_and_left_goes_on() {
    RTL.with(|r| r.set(true));
    let app = run(&[]);
    // The column lays the grid at its right, 400 to 800.
    let first = app.store().world(cell(&app, (1, 0)));
    assert_eq!((first.x, first.w), (800.0 - W, W), "at the right edge");
    // A press at the right selects the first column's cell; Left goes on.
    let press = click((800.0 - W / 2.0, at(1, 0).1));
    let app = run(&[press.clone(), key(Key::Left)]);
    let g = grid(&app);
    assert_eq!(app.store().active_descendant(g), Some(cell(&app, (2, 1))));
    let app = run(&[press, key(Key::End)]);
    RTL.with(|r| r.set(false));
    let g = grid(&app);
    assert_eq!(
        app.store().scroll(g).x,
        -(10.0 * W - 400.0),
        "scrolled to the left"
    );
    assert_eq!(app.store().active_descendant(g), Some(cell(&app, (2, 9))));
    assert_eq!(app.store().world(cell(&app, (2, 9))).x, 400.0);
}
