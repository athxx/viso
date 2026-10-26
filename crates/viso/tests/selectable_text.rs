//! A selectable label driven through the facade: a double click selects a
//! word that copy answers with, nothing typed or cut changes it, its menu
//! offers only Copy and Select All, the keyboard's focus shows its caret and
//! ring, and the focus leaving takes its selection.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{
    ClipboardReply, KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, RawText, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, Component, FlexStyle, Size};
use viso::widgets::text_input;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

/// A selectable "Hello world" over a text field.
struct NoteApp;

impl Application for NoteApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        NoteApp
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
                axis: Axis::Column,
                ..FlexStyle::default()
            },
            |cx| {
                label("Hello world")
                    .font_size(20.0)
                    .size(Size::fixed(200.0, 30.0))
                    .selectable()
                    .build(cx);
                text_input("Other").size(Size::fixed(160.0, 30.0)).build(cx);
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

fn click(x: f64, y: f64) -> [RawEvent; 2] {
    [
        mouse(x, y, RawButtons::PRIMARY, RawPhase::Down),
        mouse(x, y, RawButtons::NONE, RawPhase::Up),
    ]
}

fn key(code: Key, shift: bool) -> [RawEvent; 2] {
    let raw = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers: RawModifiers {
                shift,
                ..RawModifiers::default()
            },
        })
    };
    [raw(true), raw(false)]
}

/// Lay out, then double click on "Hello".
fn word_selected() -> Vec<RawEvent> {
    let mut script = vec![redraw(), redraw()];
    script.extend(click(10.0, 15.0));
    script.extend(click(10.0, 15.0));
    script.push(redraw());
    script
}

#[test]
fn a_double_click_selects_a_word_that_copy_answers_with_and_nothing_edits_it() {
    let copied = ClipboardReply::new();
    let cut = ClipboardReply::new();
    let mut script = word_selected();
    script.push(RawEvent::CopyRequested {
        window: WIN,
        cut: false,
        reply: copied.clone(),
    });
    script.push(RawEvent::Text(RawText {
        window: WIN,
        text: "x".into(),
    }));
    script.extend(key(Key::Backspace, false));
    script.push(RawEvent::CopyRequested {
        window: WIN,
        cut: true,
        reply: cut.clone(),
    });
    script.push(redraw());
    let app = drive_scripted::<NoteApp>(script, STEP);
    assert_eq!(copied.take().and_then(|d| d.text).as_deref(), Some("Hello"));
    assert_eq!(cut.take(), None, "a read-only text cuts nothing");
    assert_eq!(app.focused_text(), Some("Hello world"), "nor takes typing");
    assert_eq!(app.focused_selection(), Some((0, 5)));
    assert!(
        !matches!(app.text_entry(), Some(Some(_))),
        "no keyboard or input method for it"
    );
}

#[test]
fn its_context_menu_offers_only_copy_and_select_all() {
    let mut script = word_selected();
    script.push(mouse(10.0, 15.0, RawButtons::SECONDARY, RawPhase::Down));
    script.push(mouse(10.0, 15.0, RawButtons::NONE, RawPhase::Up));
    script.push(redraw());
    script.push(redraw());
    let app = drive_scripted::<NoteApp>(script, STEP);
    let menu = app.context_menu().expect("a context menu");
    assert!(menu.copy && menu.select_all);
    assert!(!menu.cut && !menu.paste);
}

#[test]
fn the_keyboard_s_focus_shows_its_caret_and_ring_and_shift_arrows_select() {
    let mut script = vec![redraw(), redraw()];
    script.extend(key(Key::Tab, false));
    script.push(redraw());
    script.extend(key(Key::Home, false));
    script.extend(key(Key::Right, true));
    script.extend(key(Key::Right, true));
    script.push(redraw());
    let app = drive_scripted::<NoteApp>(script, STEP);
    assert_eq!(app.focused_text(), Some("Hello world"));
    assert_eq!(app.focused_selection(), Some((0, 2)));
    let store = app.store();
    let text = app.root().and_then(|c| store.arena().children(c).next());
    let marks = store.field_marks(text.unwrap()).expect("marked");
    assert!(marks.ring, "the keyboard's focus wears a ring");

    let mut script = vec![redraw(), redraw()];
    script.extend(click(10.0, 15.0));
    script.push(redraw());
    let app = drive_scripted::<NoteApp>(script, STEP);
    let store = app.store();
    let text = app.root().and_then(|c| store.arena().children(c).next());
    let marks = store.field_marks(text.unwrap()).expect("marked");
    assert!(marks.caret.is_none(), "a click's focus shows no caret");
    assert!(!marks.ring);
}

#[test]
fn the_focus_leaving_takes_its_selection() {
    let mut script = word_selected();
    script.extend(key(Key::Tab, false));
    script.push(redraw());
    script.extend(key(Key::Tab, true));
    script.push(redraw());
    let app = drive_scripted::<NoteApp>(script, STEP);
    assert_eq!(app.focused_text(), Some("Hello world"), "back on the text");
    let (start, end) = app.focused_selection().expect("a selection");
    assert_eq!(start, end, "its word is no longer selected");
}
