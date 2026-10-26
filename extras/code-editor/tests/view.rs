//! What the editor draws around and over its text, driven through the
//! facade: the gutter, folds, wrapped lines, inlays, decorations and the
//! find bar.

mod common;

use common::*;
use viso_code_editor::decoration::{DIAGNOSTICS, Decoration, Severity};
use viso_code_editor::session::{Inlay, InlayKind};
use viso_code_editor::{CodeEditor, Config, Theme};

/// The lines on screen, top to bottom, each its text nodes joined.
fn lines(h: &EditorHandle) -> Vec<(usize, String)> {
    h.drawn()
        .iter()
        .map(|r| (r.line, r.text.iter().map(|t| t.3.clone()).collect()))
        .collect()
}

#[test]
fn the_gutter_numbers_the_lines_and_stays_put_as_the_text_scrolls() {
    setup(None);
    let long = "x".repeat(300);
    let text = format!("a\n{long}\nc");
    let _app = run(&text, &[focus()]);
    let h = handle();
    let theme = Theme::default();
    let drawn = h.drawn();
    let numbers: Vec<_> = drawn.iter().map(|r| r.number.clone().unwrap()).collect();
    assert_eq!(
        numbers,
        [
            ("1".to_string(), theme.line_number_current),
            ("2".to_string(), theme.line_number),
            ("3".to_string(), theme.line_number),
        ]
    );
    assert!(drawn.iter().all(|r| r.gutter_x == 0.0));
    // The text starts past the gutter.
    let (cell, _, x) = metrics();
    assert!(h.gutter_width() >= 4.0 * cell);
    assert_eq!(drawn[0].text[0].1, x);
    // The caret at the long line's end scrolls the text, not the gutter.
    let _app = run(&text, &[focus(), key(Key::Down), key(Key::End)]);
    let h = handle();
    let drawn = h.drawn();
    let scrolled = drawn[0].gutter_x;
    assert!(scrolled > 100.0, "scrolled {scrolled}");
    assert!(drawn.iter().all(|r| r.gutter_x == scrolled));
    assert_eq!(
        drawn[1].number.as_ref().unwrap().1,
        theme.line_number_current
    );
    // A hundred lines take a digit more.
    let narrow = h.gutter_width();
    let many: String = (0..120).map(|i| format!("{i}\n")).collect();
    let _app = run(&many, &[]);
    assert!((handle().gutter_width() - narrow - cell).abs() < 1.0);
}

/// The boxes of the tree filled with `color`, where they were laid out.
fn filled(app: &DrivenApp<EditorApp>, color: viso::ui::Rgba) -> Vec<viso::ui::Rect> {
    let store = app.store();
    let mut out = Vec::new();
    let mut stack: Vec<_> = app.root().into_iter().collect();
    while let Some(id) = stack.pop() {
        if store.style(id).fill == color {
            out.push(store.world(id));
        }
        stack.extend(store.arena().children(id));
    }
    out
}

#[test]
fn the_marks_a_frame_adds_are_laid_out_where_they_go() {
    setup(None);
    let theme = Theme::default();
    let (cell, lh, x) = metrics();
    // The caret a key moves and the line it lands on, each a new box.
    let app = run("ab\ncd\n", &[focus(), key(Key::Down), key(Key::Right)]);
    let carets = filled(&app, theme.caret);
    assert_eq!(carets.len(), 1, "{carets:?}");
    let c = carets[0];
    assert!(c.w > 0.0 && c.h > 0.0, "{c:?}");
    assert!((c.x - (x + cell)).abs() < 2.0, "{c:?}");
    assert!(c.y >= lh - 0.5 && c.y < 2.0 * lh, "{c:?}");
    let current = filled(&app, theme.current_line);
    assert_eq!(current.len(), 1, "{current:?}");
    assert!(
        (current[0].y - lh).abs() < 0.5 && current[0].w > 0.0,
        "{current:?}"
    );
}

#[test]
fn a_click_on_the_fold_marker_folds_and_the_caret_steps_over_the_fold() {
    setup(None);
    let text = "fn a() {\n    x;\n    y;\n}\nz";
    let (cell, lh, _) = metrics();
    // The gutter's fold room, on the first line.
    let gutter = 5.0 * cell;
    let marker = (gutter - cell * 0.8, lh * 0.5);
    let _app = run(text, &[focus()]);
    let drawn = handle().drawn();
    assert_eq!(drawn[0].fold, Some(false));
    assert_eq!(drawn[1].fold, None);
    let _app = run(text, &[focus(), click(marker), key(Key::Down)]);
    let h = handle();
    let shown = lines(&h);
    assert_eq!(
        shown.iter().map(|l| l.0).collect::<Vec<_>>(),
        [0, 3, 4],
        "the folded lines are not listed"
    );
    assert!(shown[0].1.ends_with('\u{22ef}'), "{:?}", shown[0].1);
    assert_eq!(h.drawn()[0].fold, Some(true));
    // Down from the folded line goes past what it hides.
    assert_eq!(
        h.with(|s| s.doc().line_of(s.selections().primary().head)),
        3
    );
    // Opening every fold lists the lines again.
    let _app = run(
        text,
        &[
            focus(),
            click(marker),
            key_with(Key::Digit0, primary(true, false)),
        ],
    );
    assert_eq!(lines(&handle()).len(), 5);
    // A click on a line's number selects the line.
    let _app = run(text, &[focus(), click((cell * 1.5, lh * 1.5))]);
    assert_eq!(handle().with(|s| s.selections().primary().range()), 9..16);
}

fn wrapping(e: CodeEditor) -> CodeEditor {
    e.config(Config {
        wrap: true,
        ..Config::default()
    })
}

#[test]
fn long_lines_wrap_to_the_width_and_keys_move_a_row_at_a_time() {
    setup(Some(wrapping));
    let long = "word ".repeat(60);
    let text = format!("{long}\nnext");
    let _app = run(&text, &[focus(), key(Key::Down)]);
    let h = handle();
    let drawn = h.drawn();
    let ys: std::collections::BTreeSet<i32> = drawn[0].text.iter().map(|t| t.2 as i32).collect();
    assert!(ys.len() >= 3, "the long line wraps to rows: {ys:?}");
    let rows = h.with(|s| s.row_count(0));
    assert_eq!(rows, ys.len());
    assert!(drawn[0].gutter_x == 0.0 && drawn[1].number.as_ref().unwrap().0 == "2");
    // Down stayed on the long line, a row lower.
    let (line, row) = h.with(|s| {
        let (line, row, _) = s.point_of(s.selections().primary().head);
        (line, row)
    });
    assert_eq!((line, row), (0, 1));
    setup(None);
}

fn with_inlays_and_diagnostics(e: CodeEditor) -> CodeEditor {
    e.setup(|s| {
        s.set_inlays(vec![Inlay {
            at: 5,
            text: ": Int".into(),
            kind: InlayKind::Type,
        }]);
        s.decorations_mut().set(
            DIAGNOSTICS,
            vec![Decoration::diagnostic(
                18..19,
                Severity::Error,
                "unknown name",
            )],
        );
    })
}

#[test]
fn inlays_and_diagnostics_draw_where_their_text_is() {
    setup(Some(with_inlays_and_diagnostics));
    let _app = run(
        "let a = 1\nlet b = c",
        &[focus(), key(Key::Down), key(Key::End)],
    );
    let h = handle();
    let theme = Theme::default();
    let (cell, _, x) = metrics();
    let drawn = h.drawn();
    // The inlay is a node of its own past the name, and the text after it
    // moves over by its cells.
    let texts: Vec<&str> = drawn[0].text.iter().map(|t| t.3.as_str()).collect();
    assert_eq!(texts, ["let a", ": Int", " = 1"]);
    let after = drawn[0].text[2].1;
    assert!((after - (x + (5.0 + 6.0) * cell)).abs() < 0.5, "{after}");
    assert!(drawn[0].marks.iter().any(|m| m.4 == theme.inlay_background));
    // The diagnostic squiggles under its text and marks the gutter.
    let squiggles: Vec<_> = drawn[1].over.iter().filter(|o| o.3 < 0.0).collect();
    assert_eq!(squiggles.len(), 1);
    assert_eq!(squiggles[0].4, theme.error);
    assert!((squiggles[0].1 - (x + 8.0 * cell)).abs() < 0.5);
    assert!((-squiggles[0].3 - cell).abs() < 0.5);
    assert_eq!(drawn[1].gutter_marks.len(), 1);
    assert_eq!(drawn[1].gutter_marks[0].2, theme.error);
    assert!(drawn[0].gutter_marks.is_empty());
    // The caret moves over the inlay as over nothing.
    let _app = run(
        "let a = 1\nlet b = c",
        &[focus(), key(Key::End), key(Key::Home), key(Key::Right)],
    );
    assert_eq!(carets(&handle()), [1]);
    setup(None);
}

fn alt_enter() -> RawModifiers {
    primary(false, true)
}

#[test]
fn the_find_bar_finds_steps_and_replaces() {
    setup(None);
    let text = "one two one\ntwo one\n";
    let find = key_with(Key::F, primary(false, false));
    let app = run(text, &[focus(), find.clone()]);
    let h = handle();
    let theme = Theme::default();
    assert_eq!(h.find_bar(), Some(false));
    assert_eq!(app.focused_text(), Some("one"));
    assert_eq!(h.find_count(), "1 of 3");
    assert_eq!(h.with(|s| s.selections().primary().range()), 0..3);
    let drawn = h.drawn();
    let colors: Vec<_> = drawn
        .iter()
        .flat_map(|r| r.marks.iter().map(|m| m.4))
        .filter(|c| *c == theme.find_match || *c == theme.find_current)
        .collect();
    assert_eq!(colors.len(), 3);
    // Enter steps on, Shift+Enter back, wrapping.
    let _app = run(
        text,
        &[
            focus(),
            find.clone(),
            key(Key::Enter),
            key(Key::Enter),
            key(Key::Enter),
            key_with(
                Key::Enter,
                RawModifiers {
                    shift: true,
                    ..RawModifiers::default()
                },
            ),
        ],
    );
    let h = handle();
    assert_eq!(h.with(|s| s.selections().primary().range()), 16..19);
    assert_eq!(h.find_count(), "3 of 3");
    // What is typed over the query searches for it.
    let _app = run(text, &[focus(), find.clone(), typed("two")]);
    let h = handle();
    assert_eq!(h.with(|s| s.selections().primary().range()), 4..7);
    assert_eq!(h.find_count(), "1 of 2");
    // The replace row: Enter replaces one, the accelerator with Alt all.
    let replace = if apple() {
        key_with(Key::F, primary(false, true))
    } else {
        key_with(Key::H, primary(false, false))
    };
    let app = run(
        text,
        &[
            focus(),
            replace.clone(),
            key(Key::Tab),
            typed("1"),
            key(Key::Enter),
        ],
    );
    let h = handle();
    assert_eq!(h.find_bar(), Some(true));
    assert_eq!(app.focused_text(), Some("1"));
    assert_eq!(h.text(), "1 two one\ntwo one\n");
    assert_eq!(h.with(|s| s.selections().primary().range()), 6..9);
    let _app = run(
        text,
        &[
            focus(),
            replace,
            key(Key::Tab),
            typed("1"),
            key_with(Key::Enter, alt_enter()),
        ],
    );
    let h = handle();
    assert_eq!(h.text(), "1 two 1\ntwo 1\n");
    // Escape closes the bar, and typing goes to the text again.
    let _app = run(text, &[focus(), find, key(Key::Escape), typed("Z")]);
    let h = handle();
    assert_eq!(h.find_bar(), None);
    assert!(h.with(|s| s.search().is_none()));
    assert_eq!(h.text(), "Z two one\ntwo one\n");
}
