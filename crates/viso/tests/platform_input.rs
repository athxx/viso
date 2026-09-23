//! Platform input that reaches the tree through the facade rather than a
//! widget handler: clipboard copy/cut/paste answered from the focused text
//! control's buffer, the text-entry area pushed for IME candidate windows and
//! the soft keyboard, a cancelled contact releasing capture, and the
//! one-contact rule for touch.

use std::cell::Cell;

use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{
    ClipboardReply, KeyCode, Modifiers as RawModifiers, PointerButtons as RawButtons, PointerId,
    PointerKind, PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Component, FlexStyle, PointerPhase, Size};
use viso::widgets::text_input;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    /// Pointer-downs the root saw, so a test can tell which contacts routed.
    static DOWNS: Cell<u32> = const { Cell::new(0) };
    /// Whether the root captures the pointer on press.
    static CAPTURE: Cell<bool> = const { Cell::new(false) };
}

/// A field seeded with "hello" filling the top of a counting root.
struct FieldApp;

impl Application for FieldApp {
    fn new(_cx: &mut AppCx) -> Self {
        FieldApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let root = cx.flex(
            FlexStyle {
                size: Size::fill(),
                ..FlexStyle::default()
            },
            |cx| {
                text_input("Name")
                    .value("hello")
                    .size(Size::fixed(160.0, 30.0))
                    .build(cx);
            },
        );
        let root_id = root.id();
        cx.on_pointer(root, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if p.phase == PointerPhase::Down {
                DOWNS.with(|d| d.set(d.get() + 1));
                if CAPTURE.with(Cell::get) {
                    ev.capture_pointer(root_id);
                }
            }
        });
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f64, y: f64, phase: RawPhase) -> RawEvent {
    let buttons = if phase == RawPhase::Down {
        RawButtons::PRIMARY
    } else {
        RawButtons::NONE
    };
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        x,
        y,
        buttons,
        RawModifiers::default(),
        phase,
    ))
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
        modifiers: RawModifiers::default(),
        phase,
    })
}

/// Shift+Home: select from the caret (at the end of the seed) to the start.
fn select_all() -> RawEvent {
    RawEvent::Key(RawKey {
        window: WIN,
        code: KeyCode::Home,
        pressed: true,
        repeat: false,
        modifiers: RawModifiers {
            shift: true,
            ..RawModifiers::default()
        },
    })
}

/// Lay out, click into the field, and lay out again so focus is settled.
fn focused() -> Vec<RawEvent> {
    vec![
        redraw(),
        mouse(20.0, 10.0, RawPhase::Down),
        mouse(20.0, 10.0, RawPhase::Up),
        redraw(),
    ]
}

#[test]
fn copy_answers_with_the_selection_and_leaves_the_text() {
    let reply = ClipboardReply::new();
    let mut script = focused();
    script.extend([
        select_all(),
        RawEvent::CopyRequested {
            window: WIN,
            cut: false,
            reply: reply.clone(),
        },
        redraw(),
    ]);
    let app = drive_scripted::<FieldApp>(script, STEP);
    assert_eq!(reply.take().as_deref(), Some("hello"));
    assert_eq!(app.focused_text(), Some("hello"));
}

#[test]
fn a_bare_caret_copies_nothing() {
    let reply = ClipboardReply::new();
    let mut script = focused();
    script.push(RawEvent::CopyRequested {
        window: WIN,
        cut: true,
        reply: reply.clone(),
    });
    script.push(redraw());
    let app = drive_scripted::<FieldApp>(script, STEP);
    assert_eq!(
        reply.take(),
        None,
        "an empty selection leaves the clipboard"
    );
    assert_eq!(app.focused_text(), Some("hello"), "and cuts nothing");
}

#[test]
fn cut_answers_with_the_selection_and_removes_it() {
    let reply = ClipboardReply::new();
    let mut script = focused();
    script.extend([
        select_all(),
        RawEvent::CopyRequested {
            window: WIN,
            cut: true,
            reply: reply.clone(),
        },
        redraw(),
    ]);
    let app = drive_scripted::<FieldApp>(script, STEP);
    assert_eq!(reply.take().as_deref(), Some("hello"));
    assert_eq!(app.focused_text(), Some(""));
}

#[test]
fn paste_replaces_the_selection_on_one_line() {
    let mut script = focused();
    script.extend([
        select_all(),
        RawEvent::Paste {
            window: WIN,
            text: "two\r\nlines".to_owned(),
        },
        redraw(),
    ]);
    let app = drive_scripted::<FieldApp>(script, STEP);
    assert_eq!(app.focused_text(), Some("two lines"));
}

#[test]
fn focusing_a_field_pushes_its_box_as_the_text_entry_area() {
    let app = drive_scripted::<FieldApp>(vec![redraw()], STEP);
    assert_eq!(
        app.ime_area(),
        Some(None),
        "with nothing focused the area is pushed clear once"
    );

    let app = drive_scripted::<FieldApp>(focused(), STEP);
    let area = app
        .ime_area()
        .flatten()
        .expect("a focused field has an area");
    assert_eq!((area.width, area.height), (160.0, 30.0));
}

#[test]
fn a_cancelled_contact_releases_capture() {
    CAPTURE.with(|c| c.set(true));
    let held = drive_scripted::<FieldApp>(
        vec![redraw(), touch(7, 20.0, 60.0, RawPhase::Down), redraw()],
        STEP,
    );
    assert!(held.store().capture().is_some(), "the press captured");
    let app = drive_scripted::<FieldApp>(
        vec![
            redraw(),
            touch(7, 20.0, 60.0, RawPhase::Down),
            touch(7, 20.0, 60.0, RawPhase::Cancel),
            redraw(),
        ],
        STEP,
    );
    CAPTURE.with(|c| c.set(false));
    assert_eq!(app.store().capture(), None);
}

#[test]
fn a_second_finger_does_not_route_while_the_first_is_down() {
    DOWNS.with(|d| d.set(0));
    let _ = drive_scripted::<FieldApp>(
        vec![
            redraw(),
            touch(1, 20.0, 60.0, RawPhase::Down),
            touch(2, 40.0, 60.0, RawPhase::Down),
            touch(2, 40.0, 60.0, RawPhase::Up),
            touch(1, 20.0, 60.0, RawPhase::Up),
            // The first contact lifted, so the next one is primary again.
            touch(3, 20.0, 60.0, RawPhase::Down),
            touch(3, 20.0, 60.0, RawPhase::Up),
            redraw(),
        ],
        STEP,
    );
    assert_eq!(DOWNS.with(Cell::get), 2);
}
