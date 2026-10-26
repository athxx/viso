//! Menu bars driven through the facade: a press opens a title's menu and
//! the pointer crossing another title opens its; Alt alone and F10 take the
//! keyboard to the bar, Left and Right move along it and between its menus,
//! Escape steps back; Alt and an access key open a menu and a letter in it
//! picks; the cues show while Alt is held.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted, set_alt_access};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, RawText, WindowId,
};
use viso::prelude::*;
use viso::ui::{
    Axis, BoxStyle, Content, FlexStyle, LeafStyle, NodeId, NodeStore, Role, Sizing, StateValue,
};
use viso::widgets::{menu_bar, menu_item};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
/// The middle of the box below the bar.
const FIELD: (f32, f32) = (40.0, 243.0);

thread_local! {
    static PICKS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn picked(name: &'static str) -> impl FnMut(&mut viso::ui::EventCx<'_>) {
    move |_| PICKS.with(|p| p.borrow_mut().push(name.to_owned()))
}

fn picks() -> Vec<String> {
    PICKS.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

/// A bar of File, Edit, a disabled Tools and Help along the top; a
/// focusable box below it.
struct BarApp;

impl Application for BarApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        BarApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(
            FlexStyle {
                size: Sizing::fill(),
                axis: Axis::Column,
                gap: 200.0,
                align: viso::ui::Align::Stretch,
                ..FlexStyle::default()
            },
            |cx| {
                menu_bar()
                    .menu(
                        menu_item("&File")
                            .item(menu_item("&New").on_pick(picked("new")))
                            .item(menu_item("&Open").on_pick(picked("open")))
                            .separator()
                            .item(menu_item("Save &As").on_pick(picked("save as")))
                            .item(
                                menu_item("&Recent")
                                    .item(menu_item("One").on_pick(picked("one")))
                                    .item(menu_item("Two").on_pick(picked("two"))),
                            ),
                    )
                    .menu(
                        menu_item("&Edit")
                            .item(menu_item("&Undo").on_pick(picked("undo")))
                            .item(menu_item("&Cut").on_pick(picked("cut")))
                            .item(menu_item("&Copy").on_pick(picked("copy"))),
                    )
                    .menu(
                        menu_item("&Tools")
                            .enabled(false)
                            .item(menu_item("Options")),
                    )
                    .menu(menu_item("&Help").item(menu_item("&About").on_pick(picked("about"))))
                    .build(cx);
                let field = cx.leaf(LeafStyle {
                    size: Sizing::fixed(80.0, 30.0),
                    style: BoxStyle::NONE,
                });
                cx.focusable(field, true);
                let id = field.id();
                cx.on_pointer(field, move |ev| {
                    if ev
                        .pointer()
                        .is_some_and(|p| p.phase == viso::ui::PointerPhase::Down)
                    {
                        ev.request_focus(id);
                    }
                });
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

fn click(at: (f32, f32)) -> Vec<RawEvent> {
    vec![
        mouse(at.0, at.1, RawButtons::NONE, RawPhase::Moved),
        mouse(at.0, at.1, RawButtons::PRIMARY, RawPhase::Down),
        mouse(at.0, at.1, RawButtons::PRIMARY, RawPhase::Up),
        redraw(),
        redraw(),
    ]
}

fn hover(at: (f32, f32)) -> Vec<RawEvent> {
    vec![
        mouse(at.0, at.1, RawButtons::NONE, RawPhase::Moved),
        redraw(),
    ]
}

fn raw_key(code: Key, pressed: bool, alt: bool) -> RawEvent {
    RawEvent::Key(RawKey {
        window: WIN,
        code,
        pressed,
        repeat: false,
        modifiers: RawModifiers {
            alt,
            ..RawModifiers::default()
        },
    })
}

fn key(code: Key) -> Vec<RawEvent> {
    vec![
        raw_key(code, true, false),
        raw_key(code, false, false),
        redraw(),
        redraw(),
    ]
}

/// Alt pressed and let go alone.
fn alt() -> Vec<RawEvent> {
    vec![
        raw_key(Key::AltLeft, true, true),
        raw_key(Key::AltLeft, false, false),
        redraw(),
        redraw(),
    ]
}

/// `code` pressed with Alt held.
fn alt_key(code: Key) -> Vec<RawEvent> {
    vec![
        raw_key(Key::AltLeft, true, true),
        raw_key(code, true, true),
        raw_key(code, false, true),
        raw_key(Key::AltLeft, false, false),
        redraw(),
        redraw(),
    ]
}

fn typed(text: &str) -> Vec<RawEvent> {
    vec![
        RawEvent::Text(RawText {
            window: WIN,
            text: text.to_owned(),
        }),
        redraw(),
        redraw(),
    ]
}

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<BarApp> {
    set_alt_access(Some(true));
    picks();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<BarApp>(script, STEP)
}

fn role(store: &NodeStore, id: NodeId) -> Option<Role> {
    store.semantics(id).map(|s| s.role)
}

/// The bar, its titles, each title's menu panel, and the box below.
struct Parts {
    bar: NodeId,
    titles: Vec<NodeId>,
    panels: Vec<NodeId>,
    field: NodeId,
}

fn parts(app: &DrivenApp<BarApp>) -> Parts {
    let store = app.store();
    let mut kids = store.arena().children(app.root().unwrap());
    let (bar, field) = (kids.next().unwrap(), kids.next().unwrap());
    let of = |r: Role| -> Vec<NodeId> {
        store
            .arena()
            .children(bar)
            .filter(|&k| role(store, k) == Some(r))
            .collect()
    };
    Parts {
        bar,
        titles: of(Role::MenuItem),
        panels: of(Role::Menu),
        field,
    }
}

/// The item rows of `panel`.
fn rows(store: &NodeStore, panel: NodeId) -> Vec<NodeId> {
    store
        .arena()
        .children(panel)
        .filter(|&k| role(store, k) == Some(Role::MenuItem))
        .collect()
}

fn middle(store: &NodeStore, id: NodeId) -> (f32, f32) {
    let r = store.world(id);
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

fn open(app: &DrivenApp<BarApp>) -> Vec<usize> {
    let p = parts(app);
    (0..p.panels.len())
        .filter(|&i| !app.store().hidden(p.panels[i]))
        .collect()
}

/// Whether `node`'s highlighted look shows.
fn lit(app: &DrivenApp<BarApp>, node: NodeId) -> bool {
    let look = app.store().interaction_style(node).expect("a look");
    matches!(
        look.hover_cell.and_then(|c| app.states().get(c)),
        Some(StateValue::Bool(true))
    )
}

/// How many lines a node's text draws under its glyphs: an access key's cue.
fn underlines(store: &NodeStore, node: NodeId) -> usize {
    let label = store.arena().children(node).next().unwrap_or(node);
    match store.content_payload(label) {
        Some(Content::Text { ink, .. }) => ink.as_ref().map_or(0, |i| i.lines.len()),
        _ => 0,
    }
}

fn titles() -> Vec<(f32, f32)> {
    let app = run(&[]);
    let p = parts(&app);
    p.titles.iter().map(|&t| middle(app.store(), t)).collect()
}

#[test]
fn the_bar_reads_as_a_menu_bar_of_titles() {
    let app = run(&[]);
    let store = app.store();
    let p = parts(&app);
    assert_eq!(role(store, p.bar), Some(Role::MenuBar));
    assert_eq!(p.titles.len(), 4);
    assert_eq!(p.panels.len(), 4);
    let names: Vec<String> = p
        .titles
        .iter()
        .map(|&t| store.semantics(t).and_then(|s| s.label.clone()).unwrap())
        .collect();
    assert_eq!(names, ["File", "Edit", "Tools", "Help"], "the marks gone");
    assert_eq!(
        store.semantic_state(p.titles[0]).and_then(|s| s.expanded),
        Some(false)
    );
    let a = store.world(p.titles[0]);
    let b = store.world(p.titles[1]);
    assert!(
        b.x >= a.x + a.w - 0.5 && (b.y - a.y).abs() < 0.5,
        "in a row"
    );
    assert_eq!(underlines(store, p.titles[0]), 0, "no cues yet");
}

#[test]
fn a_press_opens_a_menu_and_the_pointer_crossing_titles_follows() {
    let t = titles();
    let app = run(&[click(t[0])]);
    let store = app.store();
    let p = parts(&app);
    assert_eq!(open(&app), [0]);
    assert!(lit(&app, p.titles[0]));
    assert_eq!(
        store.semantic_state(p.titles[0]).and_then(|s| s.expanded),
        Some(true)
    );
    let title = store.world(p.titles[0]);
    let panel = store.world(p.panels[0]);
    assert!(panel.y >= title.y + title.h - 0.5, "below its title");
    assert!(
        (panel.x - title.x).abs() < 0.5,
        "at its start: {panel:?} {title:?}"
    );

    let app = run(&[click(t[0]), hover(t[1])]);
    assert_eq!(open(&app), [1], "the pointer crossing opens Edit's");
    let p = parts(&app);
    assert!(lit(&app, p.titles[1]) && !lit(&app, p.titles[0]));
    let app = run(&[click(t[0]), hover(t[2])]);
    assert_eq!(open(&app), [0], "a disabled title opens nothing");

    let probe = run(&[click(t[1])]);
    let copy = middle(
        probe.store(),
        rows(probe.store(), parts(&probe).panels[1])[2],
    );
    let app = run(&[click(t[1]), click(copy)]);
    assert_eq!(picks(), ["copy"]);
    assert!(open(&app).is_empty());

    let app = run(&[click(t[0]), click(t[0])]);
    assert!(open(&app).is_empty(), "a press on its title closes it");
}

#[test]
fn alt_alone_takes_the_keyboard_to_the_bar_and_the_arrows_walk_it() {
    let app = run(&[click(FIELD), alt()]);
    let p = parts(&app);
    assert_eq!(app.store().focused(), Some(p.titles[0]));
    assert!(lit(&app, p.titles[0]));
    assert!(underlines(app.store(), p.titles[0]) > 0, "its cues show");
    assert!(open(&app).is_empty());

    let app = run(&[alt(), key(Key::Right), key(Key::Right)]);
    let p = parts(&app);
    assert_eq!(app.store().focused(), Some(p.titles[3]), "past Tools");
    assert!(lit(&app, p.titles[3]) && !lit(&app, p.titles[1]));

    let app = run(&[alt(), key(Key::Left)]);
    assert_eq!(
        app.store().focused(),
        Some(parts(&app).titles[3]),
        "wrapping"
    );

    // Down opens the menu on its first item.
    let app = run(&[alt(), key(Key::Down)]);
    let p = parts(&app);
    assert_eq!(open(&app), [0]);
    let first = rows(app.store(), p.panels[0])[0];
    assert_eq!(app.store().focused(), Some(first));

    // Escape steps back: the menu, then the bar.
    let app = run(&[click(FIELD), alt(), key(Key::Down), key(Key::Escape)]);
    let p = parts(&app);
    assert!(open(&app).is_empty());
    assert_eq!(app.store().focused(), Some(p.titles[0]));
    let app = run(&[
        click(FIELD),
        alt(),
        key(Key::Down),
        key(Key::Escape),
        key(Key::Escape),
    ]);
    let p = parts(&app);
    assert_eq!(app.store().focused(), Some(p.field), "back where it was");
    assert!(!lit(&app, p.titles[0]));
    assert_eq!(underlines(app.store(), p.titles[0]), 0);

    // Alt again lets go.
    let app = run(&[click(FIELD), alt(), alt()]);
    assert_eq!(app.store().focused(), Some(parts(&app).field));
}

#[test]
fn left_and_right_in_a_menu_move_between_the_menus() {
    let app = run(&[alt(), key(Key::Down), key(Key::Right)]);
    let p = parts(&app);
    assert_eq!(open(&app), [1]);
    let undo = rows(app.store(), p.panels[1])[0];
    assert_eq!(app.store().focused(), Some(undo), "on its first item");

    let app = run(&[alt(), key(Key::Down), key(Key::Right), key(Key::Right)]);
    assert_eq!(open(&app), [3], "past the disabled Tools");
    let app = run(&[alt(), key(Key::Down), key(Key::Left)]);
    assert_eq!(open(&app), [3], "wrapping");

    // On an item with a submenu, the reading-direction key opens it.
    let app = run(&[alt(), key(Key::Down), key(Key::Up), key(Key::Right)]);
    let p = parts(&app);
    assert_eq!(open(&app), [0], "File stays");
    let recent = app
        .store()
        .arena()
        .children(p.panels[0])
        .find(|&k| role(app.store(), k) == Some(Role::Menu))
        .unwrap();
    assert!(!app.store().hidden(recent), "Recent opened");
}

#[test]
fn alt_and_an_access_key_open_a_menu_and_a_letter_picks() {
    let app = run(&[click(FIELD), alt_key(Key::E)]);
    let p = parts(&app);
    assert_eq!(open(&app), [1]);
    assert_eq!(
        app.store().focused(),
        Some(rows(app.store(), p.panels[1])[0])
    );

    let app = run(&[click(FIELD), alt_key(Key::F), typed("a")]);
    assert_eq!(picks(), ["save as"]);
    assert!(open(&app).is_empty());
    assert_eq!(
        app.store().focused(),
        Some(parts(&app).field),
        "back where it was"
    );

    // Two items sharing a key: the letter steps between them, Enter picks.
    let app = run(&[alt_key(Key::E), typed("c")]);
    let p = parts(&app);
    let edit = rows(app.store(), p.panels[1]);
    assert_eq!(app.store().focused(), Some(edit[1]));
    assert!(picks().is_empty());
    run(&[alt_key(Key::E), typed("c"), typed("c"), key(Key::Enter)]);
    assert_eq!(picks(), ["copy"]);

    // A disabled title's key opens nothing.
    let app = run(&[alt_key(Key::T)]);
    assert!(open(&app).is_empty());
}

#[test]
fn f10_takes_the_keyboard_to_the_bar_and_a_letter_opens_a_menu() {
    let app = run(&[click(FIELD), key(Key::F10)]);
    assert_eq!(app.store().focused(), Some(parts(&app).titles[0]));
    let app = run(&[click(FIELD), key(Key::F10), typed("h")]);
    assert_eq!(open(&app), [3]);
    run(&[click(FIELD), key(Key::F10), typed("h"), typed("a")]);
    assert_eq!(picks(), ["about"]);
}

#[test]
fn the_cues_show_while_alt_is_held() {
    let app = run(&[vec![raw_key(Key::AltLeft, true, true), redraw(), redraw()]]);
    let p = parts(&app);
    assert!(underlines(app.store(), p.titles[0]) > 0);
    assert!(app.store().focused().is_none(), "nothing moved yet");
    // Alt with a press between is no menu-bar Alt.
    let app = run(&[
        vec![raw_key(Key::AltLeft, true, true)],
        click(FIELD),
        vec![raw_key(Key::AltLeft, false, false), redraw(), redraw()],
    ]);
    let p = parts(&app);
    assert_eq!(underlines(app.store(), p.titles[0]), 0, "gone with Alt");
    assert_eq!(app.store().focused(), Some(p.field));
    // Off Alt's reach (Apple systems), Alt does nothing.
    set_alt_access(Some(false));
    let mut script = vec![redraw(), redraw()];
    script.extend(alt());
    let app = drive_scripted::<BarApp>(script, STEP);
    assert!(app.store().focused().is_none());
    set_alt_access(None);
}

thread_local! {
    static COMMANDS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

/// A captioned window with an app menu: File with New and Quit, Edit with
/// Copy.
struct AppMenuApp;

impl Application for AppMenuApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        AppMenuApp
    }

    fn menu(&self) -> Option<viso::platform::Menu> {
        use viso::platform::{Accel, Menu, MenuCommandId, SystemAction};
        Some(Menu::Main {
            items: vec![
                Menu::sub(
                    "File",
                    [
                        Menu::item("New", MenuCommandId(7), Accel::primary("n")),
                        Menu::Line,
                        Menu::system(SystemAction::Minimize),
                        Menu::system(SystemAction::Quit),
                    ],
                ),
                Menu::sub("Edit", [Menu::system(SystemAction::Copy)]),
            ],
        })
    }

    fn on_menu_command(&mut self, command: viso::platform::MenuCommandId) {
        COMMANDS.with(|c| c.borrow_mut().push(command.0));
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.leaf(LeafStyle {
            size: Sizing::fill(),
            style: BoxStyle::NONE,
        });
    }
}

fn run_app_menu(menus: bool, steps: &[Vec<RawEvent>]) -> DrivenApp<AppMenuApp> {
    set_alt_access(Some(true));
    COMMANDS.with(|c| c.borrow_mut().clear());
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    let caps = viso::platform::PlatformCaps {
        menus,
        ..viso::platform::PlatformCaps::DESKTOP
    };
    viso::__test_support::drive_scripted_with_caps::<AppMenuApp>(script, STEP, caps)
}

/// The drawn menu bar's node, if the window draws one.
fn drawn_bar(store: &NodeStore, root: NodeId) -> Option<NodeId> {
    store
        .arena()
        .children(root)
        .find(|&k| role(store, k) == Some(Role::MenuBar))
}

#[test]
fn a_window_draws_the_app_menu_where_the_platform_shows_none() {
    let app = run_app_menu(true, &[]);
    assert_eq!(
        drawn_bar(app.store(), app.root().unwrap()),
        None,
        "the system's"
    );

    let app = run_app_menu(false, &[]);
    let store = app.store();
    let root = app.root().unwrap();
    let bar = drawn_bar(store, root).expect("drawn under the caption");
    let mut kids = store.arena().children(root);
    let caption = kids.next().unwrap();
    assert_eq!(kids.next(), Some(bar));
    assert!(store.world(bar).y >= store.world(caption).y + store.world(caption).h - 0.5);
    let titles: Vec<NodeId> = store
        .arena()
        .children(bar)
        .filter(|&k| role(store, k) == Some(Role::MenuItem))
        .collect();
    assert_eq!(titles.len(), 2);
    let panel = store
        .arena()
        .children(bar)
        .find(|&k| role(store, k) == Some(Role::Menu))
        .unwrap();
    let names: Vec<String> = rows(store, panel)
        .iter()
        .map(|&r| store.semantics(r).and_then(|s| s.label.clone()).unwrap())
        .collect();
    assert_eq!(
        names,
        ["New", "Quit"],
        "what the window cannot do is left out"
    );

    // Its own access keys: Alt+F, then N, picks New.
    run_app_menu(false, &[alt_key(Key::F), typed("n")]);
    assert_eq!(COMMANDS.with(|c| c.borrow().clone()), [7]);
    // Quit closes the window.
    let app = run_app_menu(false, &[alt_key(Key::F), typed("q"), vec![redraw()]]);
    assert_eq!(app.window_count(), 0);
}
