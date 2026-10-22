//! The menu of edits a window draws where the platform draws none, driven
//! through the facade on a host with no system edit or context menu: a
//! secondary click shows the context menu at the pointer, its items copy,
//! paste and select all through the clipboard, the keyboard walks and
//! picks them, Escape and a press elsewhere close it, and a touch
//! selection's bar sits clear of the selection. The headless build shapes
//! text through its software raster, which a pointer needs to find its
//! place in the text.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted_with_caps};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PlatformCaps, PointerButtons as RawButtons,
    PointerId, PointerKind, PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Component, FlexStyle, Rect, Size};
use viso::widgets::text_input;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

/// A host with no system edit or context menu.
const BARE: PlatformCaps = PlatformCaps {
    edit_menu: false,
    context_menu: false,
    ..PlatformCaps::DESKTOP
};

/// The menu's items, in its order.
const CUT: usize = 0;
const COPY: usize = 1;
const PASTE: usize = 2;
const SELECT_ALL: usize = 3;

/// A field holding "hello wide world" at the top of the window.
struct FieldApp;

impl Application for FieldApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        FieldApp
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
                size: Size::fill(),
                ..FlexStyle::default()
            },
            |cx| {
                text_input("Note")
                    .value("hello wide world")
                    .size(Size::fixed(300.0, 30.0))
                    .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f64, y: f64, buttons: RawButtons, phase: RawPhase) -> RawEvent {
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        x,
        y,
        buttons,
        RawModifiers::default(),
        phase,
    ))
}

/// A press and release of `buttons` at `(x, y)`, then the frames that
/// settle it.
fn click(x: f64, y: f64, buttons: RawButtons) -> Vec<RawEvent> {
    vec![
        mouse(x, y, buttons, RawPhase::Down),
        mouse(x, y, RawButtons::NONE, RawPhase::Up),
        redraw(),
        redraw(),
        redraw(),
    ]
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
    vec![raw(true), raw(false), redraw(), redraw()]
}

fn drive(script: &[Vec<RawEvent>]) -> DrivenApp<FieldApp> {
    let mut all = vec![redraw()];
    all.extend(script.iter().flatten().cloned());
    drive_scripted_with_caps::<FieldApp>(all, STEP, BARE)
}

fn center(r: Rect) -> (f64, f64) {
    (f64::from(r.x + r.w * 0.5), f64::from(r.y + r.h * 0.5))
}

/// A click on item `at` of the menu `app` shows.
fn pick(app: &DrivenApp<FieldApp>, at: usize) -> Vec<RawEvent> {
    let (_, _, items) = app.drawn_menu().expect("the menu shows");
    let (x, y) = center(items[at].0);
    click(x, y, RawButtons::PRIMARY)
}

#[test]
fn a_secondary_click_shows_the_context_menu_at_the_pointer() {
    let app = drive(&[click(40.0, 12.0, RawButtons::SECONDARY)]);
    let (list, panel, items) = app.drawn_menu().expect("the menu shows");
    assert!(list, "the context menu");
    assert!((panel.x - 40.0).abs() < 0.5 && (panel.y - 12.0).abs() < 0.5);
    assert_eq!(items.len(), 4);
    assert!(items.windows(2).all(|w| w[0].0.y < w[1].0.y), "a column");
    assert!(items[PASTE].1 && items[SELECT_ALL].1);
    let (start, end) = app.focused_selection().expect("focused");
    assert_eq!(items[COPY].1, start != end, "copy only a selection");
}

#[test]
fn its_items_select_all_copy_and_paste_through_the_clipboard() {
    let mut script = vec![click(40.0, 12.0, RawButtons::SECONDARY)];
    let app = drive(&script);
    script.push(pick(&app, SELECT_ALL));
    let app = drive(&script);
    assert_eq!(app.focused_selection(), Some((0, 16)));
    assert!(app.drawn_menu().is_none(), "a pick closes it");

    script.push(click(40.0, 12.0, RawButtons::SECONDARY));
    let app = drive(&script);
    assert_eq!(app.focused_selection(), Some((0, 16)), "inside it stays");
    let (_, _, items) = app.drawn_menu().expect("the menu shows");
    assert!(items[CUT].1 && items[COPY].1 && !items[SELECT_ALL].1);
    script.push(pick(&app, COPY));

    script.push(click(295.0, 12.0, RawButtons::PRIMARY));
    script.push(click(295.0, 12.0, RawButtons::SECONDARY));
    let app = drive(&script);
    assert_eq!(app.focused_selection(), Some((16, 16)), "past the end");
    script.push(pick(&app, PASTE));
    let app = drive(&script);
    assert_eq!(app.focused_text(), Some("hello wide worldhello wide world"));
}

#[test]
fn the_keyboard_walks_and_picks_and_escape_and_a_press_elsewhere_close_it() {
    let open = click(295.0, 12.0, RawButtons::SECONDARY);
    // At the end, Paste and Select All apply: Down lands on Paste, Down
    // again on Select All.
    let app = drive(&[
        open.clone(),
        key(Key::Down),
        key(Key::Down),
        key(Key::Enter),
    ]);
    assert_eq!(app.focused_selection(), Some((0, 16)));
    assert!(app.drawn_menu().is_none());

    let app = drive(&[open.clone(), key(Key::Escape)]);
    assert!(app.drawn_menu().is_none(), "Escape");
    assert_eq!(app.focused_selection(), Some((16, 16)));

    let app = drive(&[open, click(200.0, 200.0, RawButtons::PRIMARY)]);
    assert!(app.drawn_menu().is_none(), "a press elsewhere");
}

fn touch(x: f64, y: f64, phase: RawPhase) -> RawEvent {
    let down = matches!(phase, RawPhase::Down | RawPhase::Moved);
    RawEvent::Pointer(RawPointer {
        window: WIN,
        pointer: PointerId(7),
        kind: PointerKind::Touch,
        x,
        y,
        pressure: if down { 1.0 } else { 0.0 },
        buttons: if down {
            RawButtons::PRIMARY
        } else {
            RawButtons::NONE
        },
        modifiers: Default::default(),
        phase,
    })
}

#[test]
fn a_touch_selection_shows_the_bar_clear_of_it() {
    // A double tap selects the word and shows the menu.
    let tap = |at| {
        vec![
            touch(at, 12.0, RawPhase::Down),
            touch(at, 12.0, RawPhase::Up),
            redraw(),
        ]
    };
    let mut script = vec![tap(80.0), tap(80.0), vec![redraw(), redraw()]];
    let app = drive(&script);
    let (list, panel, items) = app.drawn_menu().expect("the bar shows");
    assert!(!list, "the bar");
    assert!(panel.y >= 30.0, "no room above the field: below it");
    let shown: Vec<_> = items.iter().filter(|(_, on)| *on).collect();
    assert!(shown.windows(2).all(|w| w[0].0.x < w[1].0.x), "a row");
    assert!(!items[SELECT_ALL].1 || items[SELECT_ALL].0.w > 0.0);

    script.push(pick(&app, SELECT_ALL));
    let app = drive(&script);
    assert_eq!(app.focused_selection(), Some((0, 16)));
    assert!(app.drawn_menu().is_some(), "Select All keeps the bar");
}
