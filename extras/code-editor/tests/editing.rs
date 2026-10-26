//! The editor driven through the facade: keys, typed and composed text and
//! the pointer edit it as a user would, with many cursors, and undo takes it
//! back; the rows on screen draw the lines they hold.

mod common;

use common::*;

#[test]
fn typing_with_many_cursors_and_undoing_it() {
    let down = primary(false, true);
    let app = run(
        "alpha\nbeta\ngamma",
        &[
            // Focus and a caret at the start; then type at it.
            focus(),
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
            focus(),
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
            focus(),
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
            focus(),
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
            focus(),
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
            focus(),
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
        &[focus(), key(Key::End), preedit("ni", 2), typed("你")],
    );
    let h = handle();
    assert_eq!(h.text(), "ab你");
    assert!(h.with(|s| s.preedit().is_none()));
    assert_eq!(carets(&h), [5]);
}

#[test]
fn the_pointer_places_selects_and_adds_cursors() {
    // A click places the caret at the cell under it.
    let _app = run("hello world\nsecond", &[focus(), click(at(0, 6.0))]);
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
            focus(),
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
        &[focus(), click_with(at(1, 3.0), alt), typed("_")],
    );
    let h = handle();
    assert_eq!(h.text(), "_hello world\nsec_ond");
    // A drag selects from where it was pressed.
    let (a, b) = (at(0, 0.0), at(1, 2.0));
    let _app = run(
        "hello world\nsecond",
        &[
            // Away from the press after it, which would count as a second.
            click(at(1, 5.0)),
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
            focus(),
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
