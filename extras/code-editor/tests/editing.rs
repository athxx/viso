//! The editor driven through the facade: keys, typed and composed text and
//! the pointer edit it as a user would, with many cursors, and undo takes it
//! back; the rows on screen draw the lines they hold.

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawImePreedit, RawKey, RawPointer, RawText, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, Component, FlexStyle, Size};
use viso_code_editor::{EditorHandle, EditorSlot, code_editor};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static TEXT: RefCell<String> = const { RefCell::new(String::new()) };
    static SLOT: RefCell<Option<EditorSlot>> = const { RefCell::new(None) };
}

struct EditorApp;

impl Application for EditorApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(
            &include_bytes!("../../../crates/text/tests/fixtures/DejaVuSans-subset.ttf")[..],
        );
        EditorApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let slot = EditorSlot::default();
        SLOT.with(|s| *s.borrow_mut() = Some(slot.clone()));
        let text = TEXT.with(|t| t.borrow().clone());
        cx.flex(
            FlexStyle {
                size: Size::fill(),
                axis: Axis::Column,
                ..FlexStyle::default()
            },
            |cx| {
                code_editor(text)
                    .size(Size::fixed(600.0, 400.0))
                    .handle(&slot)
                    .build(cx);
            },
        );
    }
}

fn handle() -> EditorHandle {
    SLOT.with(|s| s.borrow().as_ref().unwrap().borrow().clone().unwrap())
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn apple() -> bool {
    viso::ui::primary_is_logo()
}

/// The platform's primary modifier, with `shift` and `alt`.
fn primary(shift: bool, alt: bool) -> RawModifiers {
    RawModifiers {
        shift,
        alt,
        logo: apple(),
        control: !apple(),
    }
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
    vec![raw(true), raw(false), redraw()]
}

fn key(code: Key) -> Vec<RawEvent> {
    key_with(code, RawModifiers::default())
}

fn typed(text: &str) -> Vec<RawEvent> {
    vec![
        RawEvent::Text(RawText {
            window: WIN,
            text: text.into(),
        }),
        redraw(),
    ]
}

fn preedit(text: &str, caret: usize) -> Vec<RawEvent> {
    vec![
        RawEvent::ImePreedit(RawImePreedit {
            window: WIN,
            text: text.into(),
            caret,
        }),
        redraw(),
    ]
}

fn mouse(x: f32, y: f32, buttons: RawButtons, phase: RawPhase, m: RawModifiers) -> RawEvent {
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        f64::from(x),
        f64::from(y),
        buttons,
        m,
        phase,
    ))
}

fn click_with(at: (f32, f32), m: RawModifiers) -> Vec<RawEvent> {
    vec![
        mouse(at.0, at.1, RawButtons::NONE, RawPhase::Moved, m),
        mouse(at.0, at.1, RawButtons::PRIMARY, RawPhase::Down, m),
        mouse(at.0, at.1, RawButtons::NONE, RawPhase::Up, m),
        redraw(),
        redraw(),
    ]
}

fn click(at: (f32, f32)) -> Vec<RawEvent> {
    click_with(at, RawModifiers::default())
}

fn run(text: &str, steps: &[Vec<RawEvent>]) -> DrivenApp<EditorApp> {
    TEXT.with(|t| *t.borrow_mut() = text.to_owned());
    let mut script = vec![redraw(), redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    script.push(redraw());
    drive_scripted::<EditorApp>(script, STEP)
}

fn carets(h: &EditorHandle) -> Vec<usize> {
    h.with(|s| s.selections().list().iter().map(|s| s.head).collect())
}

thread_local! {
    static CELL: std::cell::Cell<Option<(f32, f32)>> = const { std::cell::Cell::new(None) };
}

/// The cell's width and the line's height, measured by a run of its own.
fn cell_size() -> (f32, f32) {
    if let Some(size) = CELL.with(|c| c.get()) {
        return size;
    }
    let _app = run("", &[]);
    let size = handle().cell_size();
    CELL.with(|c| c.set(Some(size)));
    size
}

/// Where cell `col` of line `line` is on screen, at the line's middle.
fn at(line: usize, col: f32) -> (f32, f32) {
    let (cell, lh) = cell_size();
    // The editor's text starts half a font size in.
    let pad = (13.0f32 * 0.5).round();
    (pad + col * cell + cell * 0.25, line as f32 * lh + lh * 0.5)
}

#[test]
fn typing_with_many_cursors_and_undoing_it() {
    let down = primary(false, true);
    let app = run(
        "alpha\nbeta\ngamma",
        &[
            // Focus and a caret at the start; then type at it.
            click((2.0, 8.0)),
            typed("X"),
            // Two more cursors below, at the same column.
            key_with(Key::Down, down),
            key_with(Key::Down, down),
            typed("Y"),
            key(Key::Backspace),
            typed("Z"),
        ],
    );
    let h = handle();
    assert_eq!(h.text(), "XZalpha\nbZeta\ngZamma");
    assert_eq!(carets(&h), [2, 10, 16]);
    // The rows on screen draw their lines, a caret on each.
    let drawn = h.drawn();
    let lines: Vec<String> = drawn
        .iter()
        .map(|r| r.text.iter().map(|t| t.3.clone()).collect::<String>())
        .collect();
    assert_eq!(lines, ["XZalpha", "bZeta", "gZamma"]);
    for r in &drawn {
        assert_eq!(r.over.len(), 1, "a caret on line {}", r.line);
    }
    drop(app);
}

#[test]
fn undo_takes_back_a_step_for_every_cursor_and_redo_puts_it_again() {
    let down = primary(false, true);
    let undo = primary(false, false);
    let redo = primary(true, false);
    let _app = run(
        "one\ntwo",
        &[
            click((2.0, 8.0)),
            key_with(Key::Down, down),
            typed("a"),
            typed("b"),
            typed(" "),
            typed("c"),
            key_with(Key::Z, undo),
        ],
    );
    let h = handle();
    // The word after the space was a step of its own.
    assert_eq!(h.text(), "ab one\nab two");
    let _app = run(
        "one\ntwo",
        &[
            click((2.0, 8.0)),
            key_with(Key::Down, down),
            typed("a"),
            typed("b"),
            key_with(Key::Z, undo),
            key_with(Key::Z, undo),
        ],
    );
    let h = handle();
    assert_eq!(h.text(), "one\ntwo");
    assert_eq!(carets(&h), [0, 4]);
    let _app = run(
        "one\ntwo",
        &[
            click((2.0, 8.0)),
            key_with(Key::Down, down),
            typed("ab"),
            key_with(Key::Z, undo),
            key_with(Key::Z, redo),
        ],
    );
    assert_eq!(handle().text(), "abone\nabtwo");
}

#[test]
fn keys_move_break_and_indent() {
    let _app = run(
        "    a\nb",
        &[
            click((2.0, 8.0)),
            // A new line keeps the indentation of the one it breaks.
            key(Key::End),
            key(Key::Enter),
            typed("c"),
            key(Key::Down),
            key(Key::Home),
            key(Key::Tab),
            key(Key::Up),
            key(Key::Backspace),
        ],
    );
    let h = handle();
    // Up keeps the cell; Backspace in an indent goes back a tab stop.
    assert_eq!(h.text(), "    a\nc\n    b");
}

#[test]
fn a_composition_shows_inline_and_commits_once() {
    let _app = run(
        "ab",
        &[
            click((2.0, 8.0)),
            key(Key::End),
            preedit("ni", 2),
            // The composition takes the keys it is typed with.
            key(Key::Enter),
        ],
    );
    let h = handle();
    assert_eq!(h.text(), "ab");
    assert_eq!(
        h.with(|s| s.preedit().map(|p| p.text.clone())),
        Some("ni".into())
    );
    let drawn = h.drawn();
    let texts: Vec<&str> = drawn[0].text.iter().map(|t| t.3.as_str()).collect();
    assert_eq!(texts, ["ab", "ni"]);
    let _app = run(
        "ab",
        &[
            click((2.0, 8.0)),
            key(Key::End),
            preedit("ni", 2),
            typed("你"),
        ],
    );
    let h = handle();
    assert_eq!(h.text(), "ab你");
    assert!(h.with(|s| s.preedit().is_none()));
    assert_eq!(carets(&h), [5]);
}

#[test]
fn the_pointer_places_selects_and_adds_cursors() {
    // A click places the caret at the cell under it.
    let _app = run(
        "hello world\nsecond",
        &[click((2.0, 8.0)), click(at(0, 6.0))],
    );
    let h = handle();
    assert_eq!(carets(&h), [6]);
    // A double click selects the word; Alt-click adds a caret.
    let alt = RawModifiers {
        alt: true,
        ..RawModifiers::default()
    };
    let p = at(0, 7.0);
    let _app = run(
        "hello world\nsecond",
        &[
            click((2.0, 8.0)),
            vec![
                mouse(
                    p.0,
                    p.1,
                    RawButtons::PRIMARY,
                    RawPhase::Down,
                    RawModifiers::default(),
                ),
                mouse(
                    p.0,
                    p.1,
                    RawButtons::NONE,
                    RawPhase::Up,
                    RawModifiers::default(),
                ),
                mouse(
                    p.0,
                    p.1,
                    RawButtons::PRIMARY,
                    RawPhase::Down,
                    RawModifiers::default(),
                ),
                mouse(
                    p.0,
                    p.1,
                    RawButtons::NONE,
                    RawPhase::Up,
                    RawModifiers::default(),
                ),
                redraw(),
            ],
        ],
    );
    let h = handle();
    assert_eq!(h.with(|s| s.selections().primary().range()), 6..11);
    let _app = run(
        "hello world\nsecond",
        &[click((2.0, 8.0)), click_with(at(1, 3.0), alt), typed("_")],
    );
    let h = handle();
    assert_eq!(h.text(), "_hello world\nsec_ond");
    // A drag selects from where it was pressed.
    let (a, b) = (at(0, 0.0), at(1, 2.0));
    let _app = run(
        "hello world\nsecond",
        &[
            click((2.0, 8.0)),
            vec![
                mouse(
                    a.0,
                    a.1,
                    RawButtons::PRIMARY,
                    RawPhase::Down,
                    RawModifiers::default(),
                ),
                mouse(
                    b.0,
                    b.1,
                    RawButtons::PRIMARY,
                    RawPhase::Moved,
                    RawModifiers::default(),
                ),
                mouse(
                    b.0,
                    b.1,
                    RawButtons::NONE,
                    RawPhase::Up,
                    RawModifiers::default(),
                ),
                redraw(),
                redraw(),
            ],
        ],
    );
    let h = handle();
    assert_eq!(h.with(|s| s.selections().primary().range()), 0..14);
    // The selection draws over both lines.
    let drawn = h.drawn();
    assert!(drawn[0].marks.iter().any(|m| m.3 > 0.0));
    assert!(drawn[1].marks.iter().any(|m| m.3 > 0.0));
}

#[test]
fn copy_cut_and_paste_through_the_clipboard() {
    let copy = primary(false, false);
    let _app = run(
        "one\ntwo\nthree",
        &[
            click((2.0, 8.0)),
            key_with(Key::Down, RawModifiers::default()),
            // A caret's cut takes its line; the shortcut's echo cuts nothing.
            key_with(Key::X, copy),
            vec![
                RawEvent::Paste {
                    window: WIN,
                    data: viso::platform::ClipboardData::text("two\n"),
                    token: None,
                },
                redraw(),
            ],
        ],
    );
    let h = handle();
    assert_eq!(h.text(), "one\ntwo\nthree");
}
