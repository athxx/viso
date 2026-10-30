//! Items end at a line break once complete, `;` separates items on one line,
//! and a line that starts with a continuing token carries on the item before
//! it (§14). A block's last expression is its value whatever follows it, and
//! dropped where `Unit` is expected (§61).

use viso_dsl::frontend::{Origin, compile_file};
use viso_dsl::syntax::grammar::{Entry, IncrementalParse, parse_entry};
use viso_dsl::syntax::{Edit, SyntaxKind, SyntaxNode, tokenize};
use viso_dsl::{TextRange, TextSize};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
        ..Default::default()
    }
}

/// The kinds of the items `src` parses into under `entry`, with its error codes.
fn items(entry: Entry, src: &str) -> (Vec<SyntaxKind>, Vec<&'static str>) {
    let parse = parse_entry(&tokenize(src), src, entry);
    let root = SyntaxNode::new_root(parse.root);
    let kinds = root.children().iter().map(|n| n.kind()).collect();
    (kinds, parse.errors.iter().map(|d| d.code).collect())
}

fn statements(src: &str) -> Vec<SyntaxKind> {
    let (kinds, errors) = items(Entry::BlockBody, src);
    assert!(errors.is_empty(), "{src:?}: {errors:?}");
    kinds
}

/// The error codes compiling `src`, beside a component to mount, reports.
fn errors(src: &str) -> Vec<&'static str> {
    let src = format!("{src}\nexport component Main {{\n    view {{ Column {{}} }}\n}}\n");
    compile_file(&src, &origin())
        .diagnostics
        .iter()
        .filter(|d| d.severity == viso_dsl::Severity::Error)
        .map(|d| d.code)
        .collect()
}

use SyntaxKind::{AssignStmt, ExprStmt, IfStmt, LetStmt, ReturnStmt};

#[test]
fn a_line_break_ends_a_complete_item() {
    assert_eq!(
        statements("let mut i = 0\nlet mut j = 1"),
        [LetStmt, LetStmt]
    );
    assert_eq!(
        statements("let mut i = 0; let mut j = 1"),
        [LetStmt, LetStmt]
    );
    assert_eq!(statements("i += 1\nj = i"), [AssignStmt, AssignStmt]);
    // An item that is not complete goes on past the break.
    assert_eq!(statements("let x =\n    1"), [LetStmt]);
    assert_eq!(statements("if ready\n{\n}"), [IfStmt]);
}

#[test]
fn items_on_one_line_need_a_semicolon_unless_a_brace_ends_the_first() {
    let src = "let a = 1 let b = 2";
    let parse = parse_entry(&tokenize(src), src, Entry::BlockBody);
    assert_eq!(parse.errors.len(), 1);
    assert_eq!(parse.errors[0].code, "E1404");
    let fix = &parse.errors[0].fixes[0];
    assert_eq!(fix.edits[0].replacement, ";");
    assert_eq!(fix.edits[0].range.start().to_u32(), 9);

    let (kinds, errors) = items(Entry::NodeMembers, "Text {} Text {}");
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(kinds.len(), 2);
    assert_eq!(statements("if a { b() } c()"), [IfStmt, ExprStmt]);
}

#[test]
fn a_broken_item_reports_its_own_error_alone() {
    for (src, codes) in [
        ("let a =\nlet b = 2", &["E1405"][..]),
        ("let a = (1\nlet b = 2", &["E1404"][..]),
        ("f(\n", &["E1401"][..]),
    ] {
        let (_, errors) = items(Entry::BlockBody, src);
        let mut errors = errors;
        errors.sort_unstable();
        errors.dedup();
        assert_eq!(errors, codes, "{src:?}");
    }
}

#[test]
fn a_line_starting_with_a_continuing_token_carries_on() {
    for src in [
        "let x = a\n    .b()\n    ?.c",
        "let x = a\n    * b",
        "let x = a\n    && b\n    || c",
        "let x = a\n    ?? b",
        "let x = a\n    as I64",
        "let x = f(a,\n    b)",
        "let x = [1,\n    2]",
        "let p = Point {\n    x: 1,\n    y: 2,\n}",
        "let x\n    = 1",
        "if a {\n} \nelse {\n}",
    ] {
        assert_eq!(statements(src).len(), 1, "{src:?}");
    }
}

#[test]
fn a_line_starting_with_a_prefix_or_an_opener_starts_an_item() {
    assert_eq!(statements("let x = a\n-b"), [LetStmt, ExprStmt]);
    assert_eq!(statements("let x = a\n+b"), [LetStmt, ExprStmt]);
    assert_eq!(statements("f()\n(a, b)"), [ExprStmt, ExprStmt]);
    assert_eq!(statements("f()\n[a]"), [ExprStmt, ExprStmt]);
    assert_eq!(statements("return\nf()"), [ReturnStmt, ExprStmt]);
}

#[test]
fn arms_and_node_members_end_at_a_line_break() {
    assert_eq!(
        statements("match x {\n    1 => a\n    _ => b\n}"),
        [SyntaxKind::MatchStmt]
    );
    let (kinds, errors) = items(
        Entry::NodeMembers,
        "text: a\ncolor: b\nText {}\non click {\n}",
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(kinds.len(), 4);
    let (_, errors) = items(
        Entry::NodeMembers,
        "match phase {\n    A::a => { Text {} }\n    _ => {}\n}",
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn the_last_expression_is_the_value_and_unit_drops_it() {
    assert_eq!(
        errors("fn g() -> I64 { 1 }\nfn f() { g() }"),
        Vec::<&str>::new()
    );
    assert_eq!(errors("fn f() -> I64 { 1; }"), Vec::<&str>::new());
    assert_eq!(
        errors("fn f() -> I64 {\n    let a = 1\n    a + 1\n}"),
        Vec::<&str>::new()
    );
    assert_eq!(errors("fn f() -> String { 1 }"), ["E2103"]);
}

#[test]
fn an_edit_that_joins_or_splits_lines_reparses_like_a_fresh_parse() {
    let source = "fn f() {}\nfn g() {\n    let a = 1\n    let b = 2\n}\nfn h() {}\n";
    let at = |text: &str| TextSize::new(source.find(text).unwrap() as u32);
    let newline = at("\n    let b");
    let edits = [
        // Join two statements onto one line, then split them again.
        (TextRange::new(newline, newline + TextSize::new(5)), " "),
        (TextRange::new(newline, newline + TextSize::new(5)), "; "),
        // Remove the break between two functions.
        (TextRange::new(at("\nfn g"), at("fn g")), " "),
        // Continue a statement onto the next line.
        (TextRange::empty(at("\n    let b")), "\n        + 2"),
        (TextRange::empty(at("\n    let b")), "\n    - 2"),
    ];
    for (range, insert) in edits {
        let mut parse = IncrementalParse::new(source, Entry::CompilationUnit);
        let edit = Edit::new(range, insert);
        let edited = edit.apply(source);
        parse.edit(&edit, &edited);
        let fresh = IncrementalParse::new(&edited, Entry::CompilationUnit);
        assert_eq!(
            format!("{:?}", parse.parse().root),
            format!("{:?}", fresh.parse().root),
            "{edited:?}"
        );
        assert_eq!(
            format!("{:?}", parse.parse().errors),
            format!("{:?}", fresh.parse().errors),
            "{edited:?}"
        );
    }
}
