//! Parser acceptance (§152): the nesting and token budgets, the block-item
//! `if`/`match` classification, the syntax that only one context admits, the
//! lexical boundaries, keywords in label and binding positions, confusable
//! identifiers, recovery inside a component, and random edits reparsed
//! incrementally against a full parse.

use viso_dsl::edit::Document;
use viso_dsl::frontend::Origin;
use viso_dsl::syntax::grammar::{Entry, IncrementalParse, MAX_DEPTH, MAX_TOKENS, parse_entry};
use viso_dsl::syntax::{Edit, SyntaxKind, SyntaxNode, tokenize};
use viso_dsl::{Severity, TextRange, TextSize};

/// Every diagnostic code `src` gets under `entry`, lexical ones first.
fn codes(entry: Entry, src: &str) -> Vec<&'static str> {
    let tokens = tokenize(src);
    let mut out: Vec<&'static str> = tokens
        .iter()
        .filter_map(|t| t.error.map(|e| e.code()))
        .collect();
    out.extend(
        parse_entry(&tokens, src, entry)
            .errors
            .iter()
            .map(|e| e.code),
    );
    out
}

fn root(entry: Entry, src: &str) -> SyntaxNode {
    let parse = parse_entry(&tokenize(src), src, entry);
    let root = SyntaxNode::new_root(parse.root);
    assert_eq!(root.text(), src, "the tree is lossless");
    root
}

/// The kinds of the root's child nodes.
fn items(entry: Entry, src: &str) -> Vec<SyntaxKind> {
    root(entry, src)
        .children()
        .iter()
        .map(|n| n.kind())
        .collect()
}

fn has_node(entry: Entry, src: &str, kind: SyntaxKind) -> bool {
    root(entry, src)
        .descendants()
        .iter()
        .any(|n| n.kind() == kind)
}

fn clean(entry: Entry, src: &str) {
    let found = codes(entry, src);
    assert!(found.is_empty(), "{src:?} as {entry:?}: {found:?}");
}

fn rejects(entry: Entry, src: &str, code: &str) {
    let found = codes(entry, src);
    assert!(
        found.contains(&code),
        "{src:?} as {entry:?}: expected {code}, got {found:?}"
    );
}

fn rejected(entry: Entry, src: &str) {
    assert!(!codes(entry, src).is_empty(), "{src:?} as {entry:?} parsed");
}

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

/// The error codes of the whole frontend over a `.vs` source.
fn compile_errors(src: &str) -> Vec<&'static str> {
    Document::new(src, &origin())
        .diagnostics()
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

#[test]
fn nesting_past_the_depth_budget_is_one_diagnostic_not_a_stack_overflow() {
    let n = 100_000;
    let deep = [
        (Entry::Expr, format!("{}x{}", "(".repeat(n), ")".repeat(n))),
        (Entry::Expr, format!("{}1", "-".repeat(n))),
        (Entry::Expr, format!("1{}", " + 1".repeat(n))),
        (Entry::Expr, format!("a{}", ".b".repeat(n))),
        (Entry::Expr, format!("f{}", "(x)".repeat(n))),
        (Entry::Expr, format!("x{}", " as I64".repeat(n))),
        (Entry::Expr, format!("{}1", "a ?? ".repeat(n))),
        (Entry::Expr, format!("{}1{}", "[".repeat(n), "]".repeat(n))),
        (Entry::Expr, format!("{}1", "|| ".repeat(n))),
        (
            Entry::BlockBody,
            format!("{}{}", "{ ".repeat(n), "}".repeat(n)),
        ),
        (
            Entry::BlockBody,
            format!("let x: {}I64{} = 1;", "Option<".repeat(n), ">".repeat(n)),
        ),
        (
            Entry::BlockBody,
            format!("let {}x{} = 1;", "(".repeat(n), ")".repeat(n)),
        ),
        (
            Entry::BlockBody,
            format!("{}1", "if a { 1 } else ".repeat(n)),
        ),
        (
            Entry::ViewFragment,
            format!("{}{}", "Column { ".repeat(n), "}".repeat(n)),
        ),
        (
            Entry::ViewFragment,
            format!("{}T {{}}{}", "if a { ".repeat(n), "}".repeat(n)),
        ),
    ];
    for (entry, src) in &deep {
        let found = codes(*entry, src);
        let budget = found.iter().filter(|&&c| c == "E1406").count();
        assert_eq!(budget, 1, "{entry:?} {:?}…: {found:?}", &src[..24]);
        root(*entry, src);
    }
}

#[test]
fn nesting_inside_the_budget_compiles_end_to_end() {
    let depth = MAX_DEPTH as usize - 16;
    let src = format!(
        "component C {{\n    computed v: I64 = {}1{};\n    computed w: I64 = 1{};\n    view {{ Text {{ text: format(\"{{}}\", v + w); }} }}\n}}\n",
        "(".repeat(depth),
        ")".repeat(depth),
        " + 1".repeat(depth)
    );
    let errors = compile_errors(&src);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn tokens_past_the_token_budget_are_one_error_node() {
    let src = "x;\n".repeat(MAX_TOKENS / 2 + 8);
    let found = codes(Entry::BlockBody, &src);
    assert_eq!(found, ["E1407"]);
    let tree = root(Entry::BlockBody, &src);
    let last = tree.children().last().map(|n| n.kind());
    assert_eq!(last, Some(SyntaxKind::ErrorNode));
}

#[test]
fn a_leading_if_or_match_is_a_statement_never_the_head_of_a_tail() {
    use SyntaxKind::*;
    assert_eq!(
        items(Entry::BlockBody, "if a { 1 } else { 2 } - 1"),
        [IfStmt, ExprStmt]
    );
    assert_eq!(
        items(Entry::BlockBody, "match x { _ => 1 } - 1"),
        [MatchStmt, ExprStmt]
    );
    let grouped = root(Entry::BlockBody, "(if a { 1 } else { 2 }) - 1");
    assert_eq!(
        grouped
            .children()
            .iter()
            .map(|n| n.kind())
            .collect::<Vec<_>>(),
        [ExprStmt]
    );
    assert!(grouped.descendants().iter().any(|n| n.kind() == IfExpr));
    rejects(Entry::BlockBody, "match x { _ => v }.f()", "E1403");
    // In value positions the same words are expressions.
    assert!(has_node(
        Entry::BlockBody,
        "let y = if a { 1 } else { 2 };",
        IfExpr
    ));
    assert!(has_node(
        Entry::BlockBody,
        "f(match x { _ => 1 });",
        MatchExpr
    ));
    assert!(has_node(
        Entry::BlockBody,
        "return if a { 1 } else { 2 };",
        IfExpr
    ));

    // A trailing statement-form `if`/`match` gives a block its value.
    let errors = compile_errors(
        "fn pick(c: Bool) -> I64 {\n    if c { 1 } else { 2 }\n}\n\
         fn name(n: I64) -> String {\n    match n {\n        0 => \"zero\",\n        _ => \"other\",\n    }\n}\n\
         component C {\n    view { Text { text: format(\"{}{}\", pick(true), name(0)); } }\n}\n",
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn syntax_one_context_owns_is_rejected_elsewhere() {
    // `=>` only separates match arms.
    clean(Entry::Expr, "match x { 1 => a, _ => b }");
    rejected(Entry::Expr, "a => b");
    rejected(Entry::BlockBody, "let f = x => 1;");
    rejected(Entry::ComponentMembers, "computed c => 1;");
    rejects(Entry::NodeMembers, "on click => go();", "E3201");

    // `<=>` only follows `bind`.
    clean(Entry::NodeMembers, "bind value <=> settings.count;");
    rejected(Entry::NodeMembers, "value <=> settings.count;");
    rejected(Entry::BlockBody, "a <=> b;");
    rejected(Entry::Expr, "a <=> b");

    // `:` binds properties, style items and record fields; `=` assigns.
    clean(Entry::NodeMembers, "text: \"a\";");
    rejected(Entry::NodeMembers, "text = \"a\";");
    clean(
        Entry::CompilationUnit,
        "style Muted for Text { color: gray; }",
    );
    rejected(
        Entry::CompilationUnit,
        "style Muted for Text { color = gray; }",
    );
    clean(Entry::Expr, "Point { x: 1, y: 2 }");
    rejected(Entry::Expr, "Point { x = 1, y = 2 }");
    clean(Entry::BlockBody, "count = 1; count += 2;");
    rejected(Entry::BlockBody, "count: 1;");

    // A bare record expression never heads a control form.
    for head in [
        "if P { x: 1 } { }",
        "while P { x: 1 } { }",
        "for i in P { x: 1 } { }",
        "match P { x: 1 } { _ => 1 }",
    ] {
        rejects(Entry::BlockBody, head, "E2801");
    }
    clean(Entry::BlockBody, "if (P { x: 1 }) == p { }");
    for head in [
        "if P { x: 1 } { T {} }",
        "for i in P { x: 1 } key i { T {} }",
        "match P { x: 1 } { _ => T {} }",
    ] {
        rejects(Entry::ViewFragment, head, "E2801");
    }

    // A generic record constructor names its arguments with a turbofish.
    clean(Entry::Expr, "Pair::<I64> { a: 1, b: 2 }");
    rejects(Entry::Expr, "Pair<I64> { a: 1, b: 2 }", "E2004");
    rejects(Entry::Expr, "make<I64>(1)", "E2004");
    clean(Entry::Expr, "a < b");

    // `||` opens a closure where an operand starts and is `or` after one.
    assert_eq!(items(Entry::Expr, "|| 1"), [SyntaxKind::ClosureExpr]);
    assert_eq!(items(Entry::Expr, "a || b"), [SyntaxKind::BinaryExpr]);
    let mixed = root(Entry::Expr, "a || || b");
    assert_eq!(mixed.children()[0].kind(), SyntaxKind::BinaryExpr);
    assert!(
        mixed
            .descendants()
            .iter()
            .any(|n| n.kind() == SyntaxKind::ClosureExpr)
    );
    let body = root(Entry::Expr, "|x| x || y");
    assert_eq!(body.children()[0].kind(), SyntaxKind::ClosureExpr);
    assert!(
        body.descendants()
            .iter()
            .any(|n| n.kind() == SyntaxKind::BinaryExpr)
    );
}

/// The significant token kinds of `src`, with each token's lexical error code.
fn lexed(src: &str) -> Vec<(SyntaxKind, Option<&'static str>)> {
    tokenize(src)
        .iter()
        .filter(|t| !t.kind.is_trivia() && t.kind != SyntaxKind::Eof)
        .map(|t| (t.kind, t.error.map(|e| e.code())))
        .collect()
}

fn lex_code(src: &str) -> Vec<&'static str> {
    lexed(src).into_iter().filter_map(|(_, c)| c).collect()
}

#[test]
fn lexical_boundaries_hold() {
    use SyntaxKind::*;
    let kinds = |src: &str| -> Vec<SyntaxKind> {
        let tokens = lexed(src);
        assert!(
            tokens.iter().all(|(_, c)| c.is_none()),
            "{src:?}: {tokens:?}"
        );
        tokens.into_iter().map(|(k, _)| k).collect()
    };
    // `%` is a unit only where no operand continues it.
    assert_eq!(kinds("50%"), [UnitLiteral]);
    assert_eq!(kinds("50%3"), [IntLiteral, Percent, IntLiteral]);
    assert_eq!(kinds("50 % 3"), [IntLiteral, Percent, IntLiteral]);
    assert_eq!(kinds("100%-8dp"), [UnitLiteral, Minus, UnitLiteral]);
    assert_eq!(kinds("100% - 8dp"), [UnitLiteral, Minus, UnitLiteral]);
    assert_eq!(kinds("50%)"), [UnitLiteral, RParen]);
    // An exponent needs a digit; `em` is a unit.
    assert_eq!(kinds("1em"), [UnitLiteral]);
    assert_eq!(kinds("1.5em"), [UnitLiteral]);
    assert_eq!(kinds("1e5"), [FloatLiteral]);
    assert_eq!(kinds("1E-5"), [FloatLiteral]);
    // A typed suffix rides the same token kind as a unit.
    assert_eq!(kinds("1.5e+3f32"), [UnitLiteral]);
    assert_eq!(kinds("1..2"), [IntLiteral, DotDot, IntLiteral]);
    assert_eq!(lex_code("1e2dp"), ["E1203"]);

    // A separator sits only between two digits of one radix.
    for ok in [
        "1_000",
        "1_000dp",
        "1_000.000_1",
        "1e1_0",
        "0xFF_FF",
        "0b1_0",
    ] {
        assert!(lex_code(ok).is_empty(), "{ok}: {:?}", lexed(ok));
    }
    for bad in ["1_", "1__0", "1_dp", "1_.5", "1_e5", "1.5_f32", "0x_FF"] {
        assert_eq!(lex_code(bad), ["E1206"], "{bad}: {:?}", lexed(bad));
    }

    // `e` opens an exponent only before a digit; `e_5` is an unknown suffix.
    assert_eq!(lex_code("1e_5"), ["E1204"]);

    // Escapes: `\x` is ASCII only; `\u{}` holds 1–6 digits of a scalar value.
    for ok in [
        r#""\x7F""#,
        r#""\x00""#,
        r#""\u{0}""#,
        r#""\u{10FFFF}""#,
        r#""\\ \" \' \n \r \t \0""#,
        r"'\u{1F680}'",
    ] {
        assert!(lex_code(ok).is_empty(), "{ok}: {:?}", lexed(ok));
    }
    for bad in [
        r#""\x80""#,
        r#""\x7""#,
        r#""\u{110000}""#,
        r#""\u{D800}""#,
        r#""\u{}""#,
        r#""\u{1234567}""#,
        r#""\q""#,
    ] {
        assert!(!lex_code(bad).is_empty(), "{bad} lexed clean");
    }

    // Raw strings take 0 to 255 hashes and close on the same count.
    // The body holds a quote with one hash fewer than the delimiter.
    let raw = |hashes: usize| {
        format!(
            "r{0}\"a \"{1} b\"{0}",
            "#".repeat(hashes),
            "#".repeat(hashes - 1)
        )
    };
    for ok in [0, 1, 2, 255] {
        let src = if ok == 0 {
            "r\"a b\"".to_owned()
        } else {
            raw(ok)
        };
        assert_eq!(kinds(&src), [RawStringLiteral], "{ok} hashes");
    }
    assert_eq!(lex_code(&raw(256)), ["E1210"]);
}

#[test]
fn strict_keywords_are_labels_but_never_bindings() {
    // A label position takes any keyword.
    for ok in [
        "f(type: 1, match: 2)",
        "Point { type: 1, fn: 2 }",
        "a.type.match",
        "Kind::if",
    ] {
        clean(Entry::Expr, ok);
    }
    clean(Entry::BlockBody, "let Point { type: t, fn: f } = p;");
    clean(
        Entry::CompilationUnit,
        "record R { type: I64; }\nenum E { if; match(I64); }\n",
    );
    clean(Entry::NodeMembers, "type: 1; style.match: 2;");
    // A binding or declaration position rejects a strict keyword.
    for (entry, bad) in [
        (Entry::CompilationUnit, "fn f(type: I64) { }"),
        (Entry::CompilationUnit, "fn match() { }"),
        (Entry::CompilationUnit, "component if { }"),
        (Entry::CompilationUnit, "record fn { }"),
        (Entry::ComponentMembers, "state if = 1;"),
        (Entry::ComponentMembers, "input for: I64;"),
        (Entry::ComponentMembers, "action return() { }"),
        (Entry::Expr, "|fn| 1"),
        (Entry::Expr, "Point { type }"),
        (Entry::BlockBody, "let while = 1;"),
        (Entry::BlockBody, "let Point { type } = p;"),
    ] {
        rejects(entry, bad, "E1301");
    }
    // `r#` escapes it, and contextual keywords stay identifiers off their sites.
    clean(Entry::CompilationUnit, "fn f(r#type: I64) { }");
    clean(
        Entry::BlockBody,
        "let state = 1; let style = state; let key = style + 1;",
    );
    clean(
        Entry::ComponentMembers,
        "state view = 1; state input = view;",
    );
}

#[test]
fn only_identifiers_mixing_scripts_are_confusable() {
    for clean_name in [
        "café",
        "naïve_count",
        "名前",
        "名前かな",
        "ユーザーID",
        "사용자",
        "사용자_id",
        "ζήτα",
        "число",
        "_x1",
    ] {
        let src = format!("let {clean_name} = 1;");
        assert!(lex_code(&src).is_empty(), "{clean_name}: {:?}", lexed(&src));
    }
    for spoof in [
        "p\u{430}y",      // Latin p, y with Cyrillic а
        "\u{3b1}lpha",    // Greek α with Latin
        "число_x",        // Cyrillic with Latin
        "かな한",         // kana with hangul
        "\u{441}\u{3bf}", // Cyrillic beside Greek
    ] {
        let src = format!("let {spoof} = 1;");
        assert_eq!(lex_code(&src), ["E1102"], "{spoof}");
    }
}

#[test]
fn a_component_recovers_after_an_unclosed_string_or_comment() {
    let src = "component C {\n    state a = \"oops;\n    state b = 1;\n}\ncomponent D {\n    state c = 2;\n}\n";
    let found = codes(Entry::CompilationUnit, src);
    assert_eq!(found.first(), Some(&"E1201"), "{found:?}");
    let tree = root(Entry::CompilationUnit, src);
    let components: Vec<_> = tree
        .children()
        .into_iter()
        .filter(|n| n.kind() == SyntaxKind::ComponentDecl)
        .collect();
    assert_eq!(components.len(), 2, "{found:?}");
    let states = |n: &SyntaxNode| {
        n.descendants()
            .iter()
            .filter(|d| d.kind() == SyntaxKind::StateDecl)
            .count()
    };
    assert_eq!(states(&components[0]), 2);
    assert_eq!(states(&components[1]), 1);

    // An unclosed comment runs to the end; what precedes it is intact.
    let src = "component C {\n    state a = 1;\n    /* note\n    state b = 2;\n}\n";
    let found = codes(Entry::CompilationUnit, src);
    assert!(found.contains(&"E1202"), "{found:?}");
    assert!(found.contains(&"E1401"), "{found:?}");
    let tree = root(Entry::CompilationUnit, src);
    assert_eq!(states(&tree.children()[0]), 1);

    // An unclosed block inside a member keeps the next component.
    let src = "component C {\n    action go() {\n        if a {\n    }\n}\ncomponent D { state c = 2; }\n";
    let found = codes(Entry::CompilationUnit, src);
    assert!(!found.is_empty());
    root(Entry::CompilationUnit, src);
}

/// A small deterministic generator, so a failing edit sequence replays.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const SAMPLE: &str = "\
import viso::time;

record Item {
    id: I64;
    title: String = \"\";
}

export component Todo {
    input items: List<Item>;
    state draft = \"\";
    computed count: I64 = items.len();
    event added(title: String);

    action add() {
        if draft != \"\" {
            emit added(draft);
            draft = \"\";
        }
    }

    view {
        Column {
            gap: 8dp;
            for item in items key item.id {
                Text { text: item.title; }
            }
            Button { text: \"Add\"; on click { add(); } }
        }
    }
}

fn label(n: I64) -> String {
    match n {
        0 => \"none\",
        _ => format(\"{}\", n),
    }
}
";

/// Fragments an editor might type or delete, valid and broken alike.
const PIECES: &[&str] = &[
    "",
    "x",
    "1",
    " ",
    "\n",
    ";",
    "{",
    "}",
    "(",
    ")",
    "\"",
    "/*",
    "*/",
    "//",
    "if a { b }",
    "state z = 3;",
    "fn g() { }",
    "Text { }",
    ":",
    "=",
    "=>",
    "<",
    ">",
    "..",
    "#",
    "r#\"",
    "%",
    "1em",
    "é",
];

#[test]
fn random_edit_sequences_reparse_like_a_full_parse() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut reused = 0;
    let mut total = 0;
    for _ in 0..40 {
        let mut source = SAMPLE.to_owned();
        let mut parse = IncrementalParse::new(&source, Entry::CompilationUnit);
        for _ in 0..25 {
            let mut start = rng.below(source.len() + 1);
            while !source.is_char_boundary(start) {
                start -= 1;
            }
            let mut end = (start + rng.below(6)).min(source.len());
            while !source.is_char_boundary(end) {
                end += 1;
            }
            let insert = PIECES[rng.below(PIECES.len())];
            let range = TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32));
            let edit = Edit::new(range, insert);
            let edited = edit.apply(&source);
            total += 1;
            reused += usize::from(parse.edit(&edit, &edited));
            let fresh = IncrementalParse::new(&edited, Entry::CompilationUnit);
            assert_eq!(parse.tokens(), fresh.tokens(), "{edit:?} on {source:?}");
            assert_eq!(
                format!("{:?}", parse.parse().root),
                format!("{:?}", fresh.parse().root),
                "{edit:?} on {source:?}"
            );
            assert_eq!(
                format!("{:?}", parse.parse().errors),
                format!("{:?}", fresh.parse().errors),
                "{edit:?} on {source:?}"
            );
            source = edited;
        }
    }
    assert!(
        reused * 5 > total,
        "only {reused} of {total} edits in place"
    );
}
