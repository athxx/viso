//! Coarse-parser skeleton tests: the CST node shape at declaration/brace
//! granularity, structural error recovery + synchronization, multiple errors
//! per pass, and a never-hang/never-panic fuzz smoke over the parser.

use viso_dsl::syntax::{GreenChild, GreenNode, ParseErrorKind, SyntaxKind, parse, tokenize};

/// Parses `src` and returns its CST root.
fn root(src: &str) -> std::rc::Rc<GreenNode> {
    parse(&tokenize(src), src).root
}

/// The kinds of a node's non-trivia children, for structural assertions.
fn child_kinds(node: &GreenNode) -> Vec<SyntaxKind> {
    node.children()
        .iter()
        .filter(|c| !c.kind().is_trivia())
        .map(|c| c.kind())
        .collect()
}

/// The first child node of `node` whose kind is `kind`.
fn find_child(node: &GreenNode, kind: SyntaxKind) -> Option<&GreenNode> {
    node.children().iter().find_map(|c| match c {
        GreenChild::Node(n) if n.kind() == kind => Some(&**n),
        _ => None,
    })
}

#[test]
fn wraps_declarations_in_items() {
    let src = "component App { view { Text {} } } state x: 0;";
    let root = root(src);
    assert_eq!(root.kind(), SyntaxKind::Root);
    // Two top-level declarations → two Item nodes.
    assert_eq!(child_kinds(&root), [SyntaxKind::Item, SyntaxKind::Item]);
    // The tree is lossless regardless of shape.
    assert_eq!(root.text(), src);
}

#[test]
fn item_with_semicolon_terminator() {
    let src = "import foo.bar;";
    let root = root(src);
    let item = find_child(&root, SyntaxKind::Item).expect("an Item");
    // The item spans the keyword header through the terminating `;`.
    let last = item.children().last().expect("children");
    assert_eq!(last.kind(), SyntaxKind::Semi);
    assert_eq!(item.text(), src);
}

#[test]
fn nested_blocks_are_balanced() {
    let src = "component C { view { Row { Col {} } } }";
    let root = root(src);
    let item = find_child(&root, SyntaxKind::Item).expect("an Item");
    let outer = find_child(item, SyntaxKind::Block).expect("outer block");
    // The outer block contains a nested block (the `view { ... }` body).
    assert!(
        find_child(outer, SyntaxKind::Block).is_some(),
        "expected a nested Block node"
    );
    let parsed = parse(&tokenize(src), src);
    assert!(
        parsed.errors.is_empty(),
        "balanced source has no structural errors"
    );
}

#[test]
fn unclosed_block_is_flagged_but_lossless() {
    let src = "component C { view {";
    let parsed = parse(&tokenize(src), src);
    // The tree still round-trips...
    assert_eq!(parsed.root.text(), src);
    // ...and the missing closers are reported.
    assert!(
        parsed
            .errors
            .iter()
            .any(|e| e.kind == ParseErrorKind::UnclosedDelimiter),
        "expected an unclosed-delimiter error, got {:?}",
        parsed.errors
    );
}

#[test]
fn stray_closer_at_top_level_recovers() {
    let src = "} component C {}";
    let parsed = parse(&tokenize(src), src);
    assert_eq!(parsed.root.text(), src);
    // The stray `}` is an unmatched closer...
    assert!(
        parsed
            .errors
            .iter()
            .any(|e| e.kind == ParseErrorKind::UnmatchedCloser),
        "expected an unmatched-closer error, got {:?}",
        parsed.errors
    );
    // ...and the parser still recovers to parse the following declaration.
    assert!(find_child(&parsed.root, SyntaxKind::Item).is_some());
}

#[test]
fn error_run_synchronizes_and_reports_each() {
    // Two junk runs separated by a valid declaration: the parser must recover
    // between them and report both, not stop at the first.
    let src = "42 + 7 ; component Ok {} 99 * 3 ;";
    let parsed = parse(&tokenize(src), src);
    assert_eq!(parsed.root.text(), src);
    let unexpected = parsed
        .errors
        .iter()
        .filter(|e| e.kind == ParseErrorKind::UnexpectedTokens)
        .count();
    assert!(
        unexpected >= 2,
        "expected at least two error runs, got {:?}",
        parsed.errors
    );
    // The valid declaration between them is still recognized.
    assert!(find_child(&parsed.root, SyntaxKind::Item).is_some());
}

#[test]
fn every_token_lands_under_a_node() {
    // A messy mix: the round-trip proves no token was dropped during recovery.
    let corpus = [
        "component A {} } { state s: 1; @ @ @ view {",
        ";;;,,,",
        "((([[[",
        "]]])))",
        "component",  // a bare keyword with no body
        "unsafe { }", // a reserved-word item
    ];
    for src in corpus {
        assert_eq!(root(src).text(), src, "round-trip {src:?}");
    }
}

#[test]
fn never_hangs_or_panics_on_arbitrary_input() {
    // The same xorshift generator as the lexer fuzz smoke, but exercising the
    // parser: it must always terminate (no non-progress loop) and round-trip.
    let mut state: u64 = 0x243F6A8885A308D3;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    // A byte alphabet biased toward the structurally-significant characters so
    // the fuzzer hits delimiter/keyword/sync paths, not just random letters.
    let alphabet: &[u8] = b"{}[]();,: \n\t/*\"'#%.rcomponentstateview0123456789+=<>?&|";
    for _ in 0..3000 {
        let len = (next() % 48) as usize;
        let s: String = (0..len)
            .map(|_| {
                let idx = (next() as usize) % alphabet.len();
                alphabet[idx] as char
            })
            .collect();
        let parsed = parse(&tokenize(&s), &s);
        assert_eq!(parsed.root.kind(), SyntaxKind::Root);
        assert_eq!(parsed.root.text(), s, "parser must round-trip {s:?}");
    }
}
