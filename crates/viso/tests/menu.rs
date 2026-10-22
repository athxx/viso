//! Menus driven through the facade: a menu opened from its anchor by the
//! pointer and the keyboard, picks, check items, submenus that open on a
//! rest and stay open while the pointer heads for them, type-ahead, and a
//! context menu at a secondary press.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, RawText, WindowId,
};
use viso::prelude::*;
use viso::ui::{
    Axis, BoxStyle, Component, FlexStyle, Inset, LeafStyle, NodeId, NodeStore, Role, Size,
    StateValue,
};
use viso::widgets::{MenuHandleSlot, Shortcut, context_menu, menu, menu_item};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static PICKS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static SLOT: MenuHandleSlot = Rc::new(RefCell::new(None));
}

fn picked(name: &'static str) -> impl FnMut(&mut viso::ui::EventCx<'_>) {
    move |_| PICKS.with(|p| p.borrow_mut().push(name.to_owned()))
}

fn picks() -> Vec<String> {
    PICKS.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

/// A menu on an 80 by 30 anchor at 100, 100; a context region from 300 down.
struct MenuApp;

impl Application for MenuApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        MenuApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let slot = SLOT.with(Rc::clone);
        cx.flex(
            FlexStyle {
                size: Size::fill(),
                axis: Axis::Column,
                padding: Inset {
                    left: 100.0,
                    top: 100.0,
                    ..Inset::default()
                },
                gap: 170.0,
                ..FlexStyle::default()
            },
            |cx| {
                menu()
                    .anchor(|cx| {
                        cx.leaf(LeafStyle {
                            size: Size::fixed(80.0, 30.0),
                            style: BoxStyle::NONE,
                        });
                    })
                    .item(
                        menu_item("Cut")
                            .shortcut(Shortcut::primary(Key::X))
                            .on_pick(picked("cut")),
                    )
                    .item(menu_item("Copy").on_pick(picked("copy")))
                    .separator()
                    .item(menu_item("Wrap").checked(false).on_check(|_, on| {
                        PICKS.with(|p| p.borrow_mut().push(format!("wrap {on}")))
                    }))
                    .item(
                        menu_item("Share")
                            .item(menu_item("Mail").on_pick(picked("mail")))
                            .item(menu_item("Messages").on_pick(picked("messages"))),
                    )
                    .item(menu_item("Delete").enabled(false).on_pick(picked("delete")))
                    .handle(&slot)
                    .build(cx);
                context_menu()
                    .size(Size::fixed(400.0, 200.0))
                    .content(|cx| {
                        cx.leaf(LeafStyle {
                            size: Size::fill(),
                            style: BoxStyle::NONE,
                        });
                    })
                    .item(menu_item("Rename").on_pick(picked("rename")))
                    .item(menu_item("Remove").on_pick(picked("remove")))
                    .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f32, y: f32, buttons: RawButtons, phase: RawPhase) -> RawEvent {
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        f64::from(x),
        f64::from(y),
        buttons,
        RawModifiers::default(),
        phase,
    ))
}

fn click(x: f32, y: f32) -> Vec<RawEvent> {
    vec![
        mouse(x, y, RawButtons::NONE, RawPhase::Moved),
        mouse(x, y, RawButtons::PRIMARY, RawPhase::Down),
        mouse(x, y, RawButtons::PRIMARY, RawPhase::Up),
        redraw(),
    ]
}

fn hover(x: f32, y: f32) -> Vec<RawEvent> {
    vec![mouse(x, y, RawButtons::NONE, RawPhase::Moved), redraw()]
}

fn key(code: Key) -> Vec<RawEvent> {
    let raw = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers: RawModifiers::default(),
        })
    };
    vec![raw(true), raw(false), redraw()]
}

fn typed(text: &str) -> Vec<RawEvent> {
    vec![
        RawEvent::Text(RawText {
            window: WIN,
            text: text.to_owned(),
        }),
        redraw(),
    ]
}

/// Frames for `ms` milliseconds: a wakeup and a frame each step, as the
/// backend gives at a timer's deadline.
fn wait(ms: u64) -> Vec<RawEvent> {
    (0..ms.div_ceil(16) + 1)
        .flat_map(|_| [RawEvent::Wakeup, redraw()])
        .collect()
}

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<MenuApp> {
    picks();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<MenuApp>(script, STEP)
}

/// The menu's anchor box and its panels, the root first.
fn parts(app: &DrivenApp<MenuApp>) -> (NodeId, Vec<NodeId>) {
    let store = app.store();
    let menu = store.arena().children(app.root().unwrap()).next().unwrap();
    let mut kids = store.arena().children(menu);
    let anchor = kids.next().unwrap();
    let root = kids.next().unwrap();
    let mut panels = vec![root];
    let mut at = 0;
    while at < panels.len() {
        let more: Vec<NodeId> = store
            .arena()
            .children(panels[at])
            .filter(|&k| role(store, k) == Some(Role::Menu))
            .collect();
        panels.extend(more);
        at += 1;
    }
    (anchor, panels)
}

fn role(store: &NodeStore, id: NodeId) -> Option<Role> {
    store.semantics(id).map(|s| s.role)
}

/// The item rows of `panel`.
fn rows(store: &NodeStore, panel: NodeId) -> Vec<NodeId> {
    store
        .arena()
        .children(panel)
        .filter(|&k| {
            matches!(
                role(store, k),
                Some(Role::MenuItem | Role::MenuItemCheckBox | Role::MenuItemRadio)
            )
        })
        .collect()
}

fn middle(store: &NodeStore, id: NodeId) -> (f32, f32) {
    let r = store.world(id);
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

/// Whether row `row` shows highlighted: its look reads a lit cell.
fn lit(app: &DrivenApp<MenuApp>, row: NodeId) -> bool {
    let store = app.store();
    let look = store.interaction_style(row).expect("a row has a look");
    matches!(
        look.hover_cell.and_then(|c| app.states().get(c)),
        Some(StateValue::Bool(true))
    )
}

#[test]
fn a_click_on_the_anchor_opens_the_menu_below_it_and_a_click_on_an_item_picks_it() {
    let app = run(&[click(140.0, 115.0)]);
    let store = app.store();
    let (anchor, panels) = parts(&app);
    let root = panels[0];
    assert!(!store.hidden(root), "open");
    let r = store.world(root);
    assert_eq!((r.x, r.y), (100.0, 134.0), "below the anchor");
    assert!(r.w >= 180.0, "at least the least width: {r:?}");
    assert_eq!(store.focused(), Some(root), "the focus went in");
    assert_eq!(
        store.semantic_state(anchor).and_then(|s| s.expanded),
        Some(true)
    );
    assert_eq!(rows(store, root).len(), 5);
    assert_eq!(
        role(store, rows(store, root)[2]),
        Some(Role::MenuItemCheckBox)
    );
    assert!(
        panels[1..].iter().all(|&p| store.hidden(p)),
        "submenus wait"
    );

    let copy = middle(store, rows(store, root)[1]);
    let app = run(&[click(140.0, 115.0), click(copy.0, copy.1)]);
    let (anchor, panels) = parts(&app);
    assert_eq!(picks(), ["copy"]);
    assert!(app.store().hidden(panels[0]), "a pick closes it");
    assert_eq!(
        app.store().focused(),
        Some(anchor),
        "and gives the focus back"
    );
}

#[test]
fn a_disabled_item_takes_no_pick_and_a_press_outside_closes_the_menu() {
    let probe = run(&[click(140.0, 115.0)]);
    let (_, panels) = parts(&probe);
    let delete = middle(probe.store(), rows(probe.store(), panels[0])[4]);
    let app = run(&[click(140.0, 115.0), click(delete.0, delete.1)]);
    assert!(picks().is_empty());
    assert!(!app.store().hidden(parts(&app).1[0]), "still open");
    let app = run(&[click(140.0, 115.0), click(700.0, 50.0)]);
    assert!(
        app.store().hidden(parts(&app).1[0]),
        "a press outside closes it"
    );
    let app = run(&[click(140.0, 115.0), click(140.0, 115.0)]);
    assert!(
        app.store().hidden(parts(&app).1[0]),
        "the anchor toggles it"
    );
}

#[test]
fn a_press_drag_and_release_picks_in_one_gesture() {
    let probe = run(&[click(140.0, 115.0)]);
    let (_, panels) = parts(&probe);
    let cut = middle(probe.store(), rows(probe.store(), panels[0])[0]);
    run(&[vec![
        mouse(140.0, 115.0, RawButtons::NONE, RawPhase::Moved),
        mouse(140.0, 115.0, RawButtons::PRIMARY, RawPhase::Down),
        redraw(),
        mouse(cut.0, cut.1, RawButtons::PRIMARY, RawPhase::Moved),
        mouse(cut.0, cut.1, RawButtons::PRIMARY, RawPhase::Up),
        redraw(),
    ]]);
    assert_eq!(picks(), ["cut"]);
}

#[test]
fn the_keyboard_opens_moves_and_picks_and_escape_gives_the_focus_back() {
    // A click and Escape leave the focus on the anchor, closed.
    let app = run(&[click(140.0, 115.0), key(Key::Escape)]);
    let (anchor, panels) = parts(&app);
    assert!(app.store().hidden(panels[0]));
    assert_eq!(app.store().focused(), Some(anchor));

    let app = run(&[click(140.0, 115.0), key(Key::Escape), key(Key::Down)]);
    let (_, panels) = parts(&app);
    let items = rows(app.store(), panels[0]);
    assert!(lit(&app, items[0]), "Down opens it on its first item");

    // Up wraps to the last enabled item; Delete is disabled.
    let app = run(&[
        click(140.0, 115.0),
        key(Key::Escape),
        key(Key::Down),
        key(Key::Up),
    ]);
    let items = rows(app.store(), parts(&app).1[0]);
    assert!(lit(&app, items[3]), "past the disabled Delete");

    run(&[
        click(140.0, 115.0),
        key(Key::Escape),
        key(Key::Down),
        key(Key::Down),
        key(Key::Enter),
    ]);
    assert_eq!(picks(), ["copy"]);
}

#[test]
fn a_check_item_turns_over_when_picked() {
    let probe = run(&[click(140.0, 115.0)]);
    let (_, panels) = parts(&probe);
    let wrap_row = rows(probe.store(), panels[0])[2];
    let wrap = middle(probe.store(), wrap_row);
    let app = run(&[
        click(140.0, 115.0),
        click(wrap.0, wrap.1),
        click(140.0, 115.0),
    ]);
    assert_eq!(picks(), ["wrap true"]);
    let store = app.store();
    assert_eq!(
        store.semantic_state(wrap_row).and_then(|s| s.checked),
        Some(true)
    );
    let slot = store.arena().children(wrap_row).next().unwrap();
    let glyph = store.arena().children(slot).next().unwrap();
    assert!(!store.hidden(glyph), "the check shows");
}

#[test]
fn resting_on_an_item_opens_its_submenu_beside_it() {
    let probe = run(&[click(140.0, 115.0)]);
    let (_, panels) = parts(&probe);
    let share_row = rows(probe.store(), panels[0])[3];
    let share = middle(probe.store(), share_row);
    let app = run(&[click(140.0, 115.0), hover(share.0, share.1), wait(260)]);
    let store = app.store();
    let (_, panels) = parts(&app);
    let sub = panels[1];
    assert!(!store.hidden(sub), "open after the rest");
    let (s, root, row) = (
        store.world(sub),
        store.world(panels[0]),
        store.world(share_row),
    );
    assert!(
        s.x >= root.x + root.w - 6.0,
        "beside the menu: {s:?} {root:?}"
    );
    assert_eq!(s.y, row.y - 5.0, "its first item level with the item");
    assert_eq!(
        store.semantic_state(share_row).and_then(|st| st.expanded),
        Some(true)
    );
    // Not yet before the rest is over.
    let early = run(&[click(140.0, 115.0), hover(share.0, share.1), wait(60)]);
    assert!(early.store().hidden(parts(&early).1[1]));

    let mail = middle(store, rows(store, sub)[0]);
    run(&[
        click(140.0, 115.0),
        hover(share.0, share.1),
        wait(260),
        click(mail.0, mail.1),
    ]);
    assert_eq!(picks(), ["mail"]);
}

#[test]
fn a_submenu_stays_open_while_the_pointer_heads_for_it() {
    let probe = run(&[click(140.0, 115.0)]);
    let (_, panels) = parts(&probe);
    let store = probe.store();
    let share = middle(store, rows(store, panels[0])[3]);
    let delete = store.world(rows(store, panels[0])[4]);
    let open = run(&[click(140.0, 115.0), hover(share.0, share.1), wait(260)]);
    let sub = open.store().world(parts(&open).1[1]);
    // From Share down and right across Delete, toward the submenu.
    let toward = (delete.x + delete.w - 4.0, delete.y + 2.0);
    assert!(toward.0 < sub.x, "still over the menu");
    let app = run(&[
        click(140.0, 115.0),
        hover(share.0, share.1),
        wait(260),
        hover(toward.0, toward.1),
    ]);
    assert!(
        !app.store().hidden(parts(&app).1[1]),
        "kept open on the way"
    );
    // Straight down to Delete's start, away from it, closes it.
    let away = (delete.x + 10.0, delete.y + delete.h / 2.0);
    let app = run(&[
        click(140.0, 115.0),
        hover(share.0, share.1),
        wait(260),
        hover(away.0, away.1),
    ]);
    assert!(app.store().hidden(parts(&app).1[1]), "closed moving away");
}

#[test]
fn the_keyboard_goes_into_a_submenu_and_back() {
    let app = run(&[
        click(140.0, 115.0),
        key(Key::Escape),
        key(Key::Up),
        key(Key::Right),
    ]);
    let (_, panels) = parts(&app);
    let store = app.store();
    assert!(!store.hidden(panels[1]), "Right opens the submenu");
    assert_eq!(store.focused(), Some(panels[1]));
    assert!(lit(&app, rows(store, panels[1])[0]));
    let app = run(&[
        click(140.0, 115.0),
        key(Key::Escape),
        key(Key::Up),
        key(Key::Right),
        key(Key::Left),
    ]);
    let (_, panels) = parts(&app);
    assert!(app.store().hidden(panels[1]), "Left goes back");
    assert_eq!(app.store().focused(), Some(panels[0]));
    assert!(lit(&app, rows(app.store(), panels[0])[3]), "to Share");
    let app = run(&[
        click(140.0, 115.0),
        key(Key::Escape),
        key(Key::Up),
        key(Key::Right),
        key(Key::Escape),
    ]);
    let (_, panels) = parts(&app);
    let store = app.store();
    assert!(
        store.hidden(panels[1]) && !store.hidden(panels[0]),
        "Escape closes one level"
    );
}

#[test]
fn typing_jumps_to_the_items_it_starts() {
    let app = run(&[click(140.0, 115.0), typed("c")]);
    let items = rows(app.store(), parts(&app).1[0]);
    assert!(lit(&app, items[0]), "c: Cut");
    let app = run(&[click(140.0, 115.0), typed("c"), typed("c")]);
    let items = rows(app.store(), parts(&app).1[0]);
    assert!(lit(&app, items[1]), "c again: Copy");
    let app = run(&[click(140.0, 115.0), typed("s"), typed("h")]);
    let items = rows(app.store(), parts(&app).1[0]);
    assert!(lit(&app, items[3]), "sh: Share");
}

#[test]
fn a_secondary_press_opens_the_context_menu_there() {
    let press = (150.0, 400.0);
    let secondary = |phase| mouse(press.0, press.1, RawButtons::SECONDARY, phase);
    let app = run(&[vec![
        mouse(press.0, press.1, RawButtons::NONE, RawPhase::Moved),
        secondary(RawPhase::Down),
        secondary(RawPhase::Up),
        redraw(),
        redraw(),
    ]]);
    let store = app.store();
    let region = store.arena().children(app.root().unwrap()).nth(1).unwrap();
    let panel = store
        .arena()
        .children(region)
        .find(|&k| role(store, k) == Some(Role::Menu))
        .unwrap();
    assert!(!store.hidden(panel));
    let r = store.world(panel);
    assert_eq!((r.x, r.y), press, "at the press");
    let rename = middle(store, rows(store, panel)[0]);
    run(&[
        vec![
            mouse(press.0, press.1, RawButtons::NONE, RawPhase::Moved),
            secondary(RawPhase::Down),
            secondary(RawPhase::Up),
            redraw(),
        ],
        click(rename.0, rename.1),
    ]);
    assert_eq!(picks(), ["rename"]);
    // A press elsewhere in the region closes it.
    let app = run(&[
        vec![
            mouse(press.0, press.1, RawButtons::NONE, RawPhase::Moved),
            secondary(RawPhase::Down),
            secondary(RawPhase::Up),
            redraw(),
        ],
        click(450.0, 450.0),
    ]);
    let store = app.store();
    let region = store.arena().children(app.root().unwrap()).nth(1).unwrap();
    let panel = store
        .arena()
        .children(region)
        .find(|&k| role(store, k) == Some(Role::Menu))
        .unwrap();
    assert!(store.hidden(panel), "a press outside the menu closes it");
}
