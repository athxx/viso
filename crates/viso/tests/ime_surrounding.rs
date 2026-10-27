//! The text around a field's caret that the input method reads, and the
//! edits it makes there: an autocorrect replacing a typed word, a
//! reconversion composing over committed text, deletions around a
//! composition, a selection it moves, and the clauses it marks.

use std::cell::Cell;
use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{
    ImeEdit, Modifiers as RawModifiers, PointerButtons as RawButtons, PointerPhase as RawPhase,
    RawEvent, RawImeEdit, RawImePreedit, RawPointer, RawText, SurroundingText, WindowId,
};
use viso::prelude::*;
use viso::ui::{FlexStyle, Sizing};
use viso::widgets::text_input;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    /// Whether the field masks its text.
    static SECURE: Cell<bool> = const { Cell::new(false) };
}

/// A field seeded with "say teh" at the top of the window.
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
                text_input("Name")
                    .value("say teh")
                    .secure(SECURE.with(Cell::get))
                    .size(Sizing::fixed(160.0, 30.0))
                    .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(phase: RawPhase) -> RawEvent {
    let buttons = if phase == RawPhase::Down {
        RawButtons::PRIMARY
    } else {
        RawButtons::NONE
    };
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        150.0,
        10.0,
        buttons,
        RawModifiers::default(),
        phase,
    ))
}

/// Lay out, click into the field past its text, and lay out again.
fn focused() -> Vec<RawEvent> {
    vec![
        redraw(),
        mouse(RawPhase::Down),
        mouse(RawPhase::Up),
        redraw(),
    ]
}

fn edit(edit: ImeEdit) -> RawEvent {
    RawEvent::ImeEdit(RawImeEdit { window: WIN, edit })
}

fn preedit(text: &str, caret: usize) -> RawEvent {
    RawEvent::ImePreedit(RawImePreedit {
        window: WIN,
        text: text.into(),
        caret,
        clauses: viso::platform::PreeditClauses::default(),
    })
}

fn commit(text: &str) -> RawEvent {
    RawEvent::Text(RawText {
        window: WIN,
        text: text.into(),
    })
}

fn shown(text: &str, anchor: usize, focus: usize) -> SurroundingText {
    SurroundingText {
        text: text.into(),
        offset: 0,
        anchor,
        focus,
        composition: None,
    }
}

#[test]
fn a_focused_field_shows_the_input_method_its_text() {
    let app = drive_scripted::<FieldApp>(focused(), STEP);
    assert_eq!(app.surrounding_text(), Some(&shown("say teh", 7, 7)));
}

#[test]
fn an_autocorrect_replaces_the_typed_word() {
    let mut events = focused();
    events.extend([
        edit(ImeEdit::Replace {
            range: 4..7,
            text: "the".into(),
        }),
        redraw(),
    ]);
    let app = drive_scripted::<FieldApp>(events, STEP);
    assert_eq!(app.focused_text(), Some("say the"));
    assert_eq!(app.surrounding_text(), Some(&shown("say the", 7, 7)));
}

#[test]
fn a_reconversion_composes_over_committed_text() {
    let mut events = focused();
    events.extend([
        edit(ImeEdit::Recompose {
            range: 4..7,
            text: "てh".into(),
            caret: 4,
        }),
        redraw(),
    ]);
    let app = drive_scripted::<FieldApp>(events.clone(), STEP);
    assert_eq!(app.focused_text(), Some("say てh"));
    assert_eq!(
        app.surrounding_text(),
        Some(&SurroundingText {
            composition: Some((4, 8)),
            ..shown("say てh", 8, 8)
        })
    );
    events.extend([commit("手"), redraw()]);
    let app = drive_scripted::<FieldApp>(events, STEP);
    assert_eq!(app.focused_text(), Some("say 手"));
    assert_eq!(app.surrounding_text(), Some(&shown("say 手", 7, 7)));
}

#[test]
fn deleting_before_a_composition_keeps_it_composing() {
    let mut events = focused();
    events.extend([
        preedit("x", 1),
        redraw(),
        edit(ImeEdit::Replace {
            range: 3..4,
            text: String::new(),
        }),
        redraw(),
    ]);
    let app = drive_scripted::<FieldApp>(events.clone(), STEP);
    assert_eq!(app.focused_text(), Some("saytehx"));
    assert_eq!(
        app.surrounding_text().and_then(|s| s.composition),
        Some((6, 7))
    );
    events.extend([commit("y"), redraw()]);
    let app = drive_scripted::<FieldApp>(events, STEP);
    assert_eq!(app.focused_text(), Some("saytehy"));
}

#[test]
fn the_input_method_moves_the_selection() {
    let mut events = focused();
    events.extend([
        edit(ImeEdit::Select {
            anchor: 0,
            focus: 3,
        }),
        redraw(),
    ]);
    let app = drive_scripted::<FieldApp>(events, STEP);
    assert_eq!(app.focused_selection(), Some((0, 3)));
    assert_eq!(app.surrounding_text(), Some(&shown("say teh", 0, 3)));
}

#[test]
fn a_secret_shows_no_text() {
    SECURE.with(|s| s.set(true));
    let app = drive_scripted::<FieldApp>(focused(), STEP);
    SECURE.with(|s| s.set(false));
    assert!(app.text_entry().flatten().is_some());
    assert_eq!(app.surrounding_text(), None);
}

/// "にほんご" composed as two clauses, the second converting.
#[cfg(feature = "headless")]
fn clauses() -> RawEvent {
    RawEvent::ImePreedit(RawImePreedit {
        window: WIN,
        text: "にほんご".into(),
        caret: 12,
        clauses: viso::platform::PreeditClauses {
            clauses: vec![(0, 6), (6, 12)],
            selected: Some((6, 12)),
        },
    })
}

#[cfg(feature = "headless")]
fn assert_two_clauses(underlines: &[viso::ui::Rect]) {
    let [first, second] = underlines else {
        panic!("one underline a clause: {underlines:?}");
    };
    assert!(
        first.x + first.w < second.x,
        "a gap between: {underlines:?}"
    );
    assert_eq!(first.y, second.y);
    assert!(second.h > first.h, "the converting clause is thicker");
}

/// Drawn marks need text shaped, which the headless build's raster does.
#[cfg(feature = "headless")]
#[test]
fn each_clause_is_underlined_and_the_converting_one_thicker() {
    let mut events = focused();
    events.extend([clauses(), redraw()]);
    let app = drive_scripted::<FieldApp>(events.clone(), STEP);
    assert_eq!(app.focused_text(), Some("say tehにほんご"));
    assert_two_clauses(app.focused_underlines().unwrap_or_default());
    events.extend([preedit("にほんご", 12), redraw()]);
    let app = drive_scripted::<FieldApp>(events, STEP);
    assert_eq!(app.focused_underlines().map(<[_]>::len), Some(1));
}

#[cfg(feature = "headless")]
#[test]
fn a_recorded_composition_replays_its_clauses() {
    use viso::__test_support::{UiTape, drive_recording, drive_tape};

    let mut events = focused();
    events.extend([clauses(), redraw()]);
    let (_recorded, tape) = drive_recording::<FieldApp>(events, STEP);
    let read = UiTape::from_bytes(&tape.to_bytes()).expect("decodes");
    assert_eq!(read, tape);
    let app = drive_tape::<FieldApp>(read);
    assert_two_clauses(app.focused_underlines().unwrap_or_default());
}
