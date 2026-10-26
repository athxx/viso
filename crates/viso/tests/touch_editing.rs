//! Touch editing driven through the facade: a long press selects the word
//! under the finger and its lift shows the platform's edit menu, the menu's
//! Select All selects the whole text and keeps it, a menu that closes on its
//! own is not shown again, and a tap on the selection hides it.
//! The headless build shapes text through its software raster, which a
//! pointer needs to find its place in the text.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{
    EditMenuAction, PointerButtons as RawButtons, PointerId, PointerKind, PointerPhase as RawPhase,
    RawEvent, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Component, FlexStyle, Sizing};
use viso::widgets::text_input;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

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
                size: Sizing::fill(),
                ..FlexStyle::default()
            },
            |cx| {
                text_input("Note")
                    .value("hello wide world")
                    .size(Sizing::fixed(300.0, 30.0))
                    .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn touch(id: u64, x: f64, y: f64, phase: RawPhase) -> RawEvent {
    let down = matches!(phase, RawPhase::Down | RawPhase::Moved);
    RawEvent::Pointer(RawPointer {
        window: WIN,
        pointer: PointerId(id),
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

/// A finger held still at `(x, y)` past the long press, then lifted. A
/// waiting timer sets no redraw reason, so each beat wakes the loop first.
fn long_press(id: u64, x: f64, y: f64) -> Vec<RawEvent> {
    let mut script = vec![touch(id, x, y, RawPhase::Down), redraw()];
    for _ in 0..40 {
        script.extend([RawEvent::Wakeup, redraw()]);
    }
    script.push(touch(id, x, y, RawPhase::Up));
    script.push(redraw());
    script
}

fn menu(action: EditMenuAction) -> RawEvent {
    RawEvent::EditMenu {
        window: WIN,
        action,
    }
}

#[test]
fn a_long_press_selects_and_its_lift_shows_the_edit_menu() {
    let mut script = vec![redraw()];
    script.extend(long_press(1, 2.0, 12.0));
    let app = drive_scripted::<FieldApp>(script, STEP);
    let selection = app.focused_selection().expect("the field took the focus");
    let menu = app.edit_menu().expect("the menu shows");
    if viso::platform::primary_is_logo() {
        assert_eq!(
            selection.0, selection.1,
            "on Apple the caret: {selection:?}"
        );
        assert!(!menu.copy && menu.select_all);
    } else {
        assert_eq!(selection, (0, 5), "the word");
        assert!(menu.copy && menu.cut && menu.paste && menu.select_all);
    }
    assert!(menu.at.height > 0.0, "{menu:?}");
}

#[test]
fn select_all_keeps_the_menu_and_a_tap_on_the_selection_hides_it() {
    let mut script = vec![redraw()];
    script.extend(long_press(1, 2.0, 12.0));
    script.push(menu(EditMenuAction::SelectAll));
    script.push(redraw());
    let app = drive_scripted::<FieldApp>(script.clone(), STEP);
    assert_eq!(app.focused_selection(), Some((0, 16)));
    let menu_after = app.edit_menu().expect("the menu stays");
    assert!(menu_after.copy && !menu_after.select_all);

    let mut closed = script.clone();
    closed.push(menu(EditMenuAction::Closed));
    closed.push(redraw());
    let app = drive_scripted::<FieldApp>(closed, STEP);
    assert_eq!(app.edit_menu(), None, "closed on its own");
    assert_eq!(
        app.focused_selection(),
        Some((0, 16)),
        "the selection stays"
    );

    let mut tapped = script;
    // Inside the selection, away from the handles at its ends.
    tapped.extend([
        touch(2, 60.0, 12.0, RawPhase::Down),
        touch(2, 60.0, 12.0, RawPhase::Up),
        redraw(),
    ]);
    let app = drive_scripted::<FieldApp>(tapped, STEP);
    assert_eq!(
        app.edit_menu(),
        None,
        "a tap on the selection hides the menu"
    );
    assert_eq!(app.focused_selection(), Some((0, 16)), "and keeps it");
}

#[test]
fn the_loupe_follows_the_finger_carrying_the_caret_and_goes_with_the_lift() {
    let mut held = vec![redraw()];
    let mut press = long_press(1, 2.0, 12.0);
    let lift = press.split_off(press.len() - 2);
    held.extend(press);
    held.extend([touch(1, 60.0, 14.0, RawPhase::Moved), redraw()]);
    let app = drive_scripted::<FieldApp>(held.clone(), STEP);
    let loupe = app
        .loupe()
        .expect("the loupe shows while the finger carries");
    assert!((loupe.x - 60.0).abs() < 0.5 && (loupe.y - 14.0).abs() < 0.5);
    assert!(loupe.caret.height > 0.0 && loupe.caret.x > 2.0, "{loupe:?}");
    assert!(app.edit_menu().is_none(), "no menu while carried");

    held.extend(lift);
    let app = drive_scripted::<FieldApp>(held, STEP);
    assert_eq!(app.loupe(), None, "the lift puts it away");
    assert!(app.edit_menu().is_some());
}
