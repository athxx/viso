//! `.vs` highlighted through the facade: the rows draw each token in its
//! theme color, and an edit that opens or closes a comment recolors the
//! lines below it.

mod common;

use std::rc::Rc;

use common::*;
use viso_code_editor::highlight::TokenKind;
use viso_code_editor::lang::vs::Vs;
use viso_code_editor::{CodeEditor, Theme};

fn vs(editor: CodeEditor) -> CodeEditor {
    editor.language(Rc::new(Vs))
}

/// The color the row of line `line` draws byte `col` of its text in.
fn color_at(h: &EditorHandle, line: usize, col: usize) -> viso::ui::Rgba {
    let row = h
        .drawn()
        .into_iter()
        .find(|r| r.line == line)
        .expect("the line is on screen");
    let mut at = 0;
    for (t, colors) in row.text.iter().zip(&row.colors) {
        if col < at + t.3.len() {
            return colors[col - at];
        }
        at += t.3.len();
    }
    panic!("line {line} has no byte {col}");
}

#[test]
fn tokens_draw_in_their_theme_colors() {
    setup(Some(vs));
    let _app = run(
        "component A {\n    state n: Int = 3; // count\n    let s = \"a\\n\";\n}",
        &[],
    );
    let h = handle();
    let theme = Theme::dark();
    let look = |k| theme.token(k).color;
    assert_eq!(color_at(&h, 0, 0), look(TokenKind::Keyword));
    assert_eq!(color_at(&h, 0, 10), look(TokenKind::Type));
    assert_eq!(color_at(&h, 1, 4), look(TokenKind::Keyword));
    assert_eq!(color_at(&h, 1, 13), look(TokenKind::Type));
    assert_eq!(color_at(&h, 1, 19), look(TokenKind::Number));
    assert_eq!(color_at(&h, 1, 22), look(TokenKind::Comment));
    assert_eq!(color_at(&h, 2, 12), look(TokenKind::String));
    assert_eq!(color_at(&h, 2, 14), look(TokenKind::Escape));
    setup(None);
}

#[test]
fn opening_a_comment_recolors_the_lines_below_and_closing_it_restores_them() {
    setup(Some(vs));
    let text = "let a = 1;\nlet b = 2;\nlet c = 3;\nlet d = 4;";
    let theme = Theme::dark();
    let comment = theme.token(TokenKind::Comment).color;
    let keyword = theme.token(TokenKind::Keyword).color;
    // `/*` typed at the start of the first line.
    let _app = run(text, &[click((2.0, 8.0)), typed("/"), typed("*")]);
    let h = handle();
    assert_eq!(h.text(), "/*let a = 1;\nlet b = 2;\nlet c = 3;\nlet d = 4;");
    for line in 0..4 {
        assert_eq!(
            color_at(&h, line, 3),
            comment,
            "line {line} is in the comment"
        );
    }
    // And `*/` typed at the end of the second: the lines after it are code.
    let _app = run(
        text,
        &[
            click((2.0, 8.0)),
            typed("/"),
            typed("*"),
            key(Key::Down),
            key(Key::End),
            typed("*"),
            typed("/"),
        ],
    );
    let h = handle();
    assert_eq!(color_at(&h, 1, 0), comment);
    assert_eq!(color_at(&h, 2, 0), keyword);
    assert_eq!(color_at(&h, 3, 0), keyword);
    setup(None);
}
