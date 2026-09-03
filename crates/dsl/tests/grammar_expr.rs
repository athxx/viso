//! Commit-1 grammar tests: the precedence-correct expression parser and the
//! red-tree navigation layer built over the typed CST.
//!
//! The expression parser is exercised through the compilation-unit entry, whose
//! commit-1 placeholder body parses each top-level construct as an expression
//! statement, so a bare expression like `a + b * c` parses to an `ExprStmt`
//! wrapping the expression tree. These tests assert the *shape* of that tree
//! (associativity, precedence, postfix folding), the non-associativity and
//! record-in-head diagnostics, and that the red tree navigates the same tree
//! with absolute positions while staying byte-for-byte lossless.

use viso_dsl::syntax::grammar::parse;
use viso_dsl::syntax::{ParseErrorKind, SyntaxKind, SyntaxNode, tokenize};

/// Parses `src` through the compilation-unit entry and returns the red root.
fn red_root(src: &str) -> SyntaxNode {
    let parse = parse(&tokenize(src), src);
    SyntaxNode::new_root(parse.root)
}

/// Whether `src` produces at least one structural error of `kind`.
fn has_error(src: &str, kind: ParseErrorKind) -> bool {
    parse(&tokenize(src), src)
        .errors
        .iter()
        .any(|e| e.kind == kind)
}

/// The first descendant node of `root` whose kind is `kind`.
fn first(root: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    root.descendants().into_iter().find(|n| n.kind() == kind)
}

#[test]
fn root_is_a_compilation_unit_and_lossless() {
    let src = "a + b;";
    let root = red_root(src);
    assert_eq!(root.kind(), SyntaxKind::CompilationUnit);
    // The red root reconstructs the source byte-for-byte, trivia included.
    assert_eq!(root.text(), src);
}

#[test]
fn multiplication_binds_tighter_than_addition() {
    // `a + b * c` must nest the multiplication under the addition's right side.
    let root = red_root("a + b * c");
    let add = first(&root, SyntaxKind::BinaryExpr).expect("a BinaryExpr");
    // The outer binary is the addition; its second operand is the multiplication.
    let operands: Vec<SyntaxNode> = add.children();
    assert_eq!(add.kind(), SyntaxKind::BinaryExpr);
    // A tighter `*` binds into a nested BinaryExpr under the `+`.
    let nested = add
        .descendants()
        .into_iter()
        .filter(|n| n.kind() == SyntaxKind::BinaryExpr)
        .count();
    assert_eq!(nested, 2, "one `+` wrapping one `*`, got {operands:?}");
}

#[test]
fn left_associative_subtraction_nests_to_the_left() {
    // `a - b - c` parses as `(a - b) - c`: the left operand is itself a binary.
    let root = red_root("a - b - c");
    let outer = first(&root, SyntaxKind::BinaryExpr).expect("outer BinaryExpr");
    let first_child = outer.first_child().expect("a left operand");
    assert_eq!(
        first_child.kind(),
        SyntaxKind::BinaryExpr,
        "left operand of a left-assoc chain is itself a BinaryExpr"
    );
}

#[test]
fn postfix_call_and_field_fold_left() {
    // `a.b(c)` is a call whose callee is a field access on `a`.
    let root = red_root("a.b(c)");
    let call = first(&root, SyntaxKind::CallExpr).expect("a CallExpr");
    let callee = call.first_child().expect("callee");
    assert_eq!(callee.kind(), SyntaxKind::FieldExpr, "callee is `a.b`");
}

#[test]
fn turbofish_call_is_a_call_expr() {
    let root = red_root("make::<Foo>(x)");
    let call = first(&root, SyntaxKind::CallExpr).expect("a CallExpr");
    assert!(
        first(&call, SyntaxKind::GenericCallArgs).is_some(),
        "the call carries turbofish generic args"
    );
}

#[test]
fn comparison_chain_is_non_associative() {
    assert!(
        has_error("a < b < c", ParseErrorKind::NonAssocChain),
        "chained comparison must be diagnosed E2701"
    );
    // A single comparison is fine.
    assert!(!has_error("a < b", ParseErrorKind::NonAssocChain));
}

#[test]
fn range_chain_is_non_associative() {
    assert!(
        has_error("a .. b .. c", ParseErrorKind::NonAssocRange),
        "chained range must be diagnosed E2702"
    );
}

#[test]
fn record_expression_in_control_flow_head_is_diagnosed() {
    // `if P { .. } { .. }` — the record body collides with the if's block, so a
    // bare record in the head is E2801.
    assert!(
        has_error("if Foo { x: 1 } { }", ParseErrorKind::RecordExprInHead),
        "a bare record in an if head must be diagnosed E2801"
    );
    // In normal position a record parses without that diagnostic.
    let root = red_root("Foo { x: 1 }");
    assert!(first(&root, SyntaxKind::RecordExpr).is_some());
    assert!(!has_error("Foo { x: 1 }", ParseErrorKind::RecordExprInHead));
}

#[test]
fn cast_expression_parses_a_type_operand() {
    let root = red_root("x as Foo");
    let cast = first(&root, SyntaxKind::CastExpr).expect("a CastExpr");
    assert!(
        first(&cast, SyntaxKind::TypePath).is_some(),
        "the cast's right operand is a type path"
    );
}

#[test]
fn red_tree_navigation_round_trips() {
    let src = "a + b * c";
    let root = red_root(src);
    let mul = first(&root, SyntaxKind::BinaryExpr)
        .and_then(|_| {
            root.descendants()
                .into_iter()
                .rfind(|n| n.kind() == SyntaxKind::BinaryExpr)
        })
        .expect("the inner `*` BinaryExpr");

    // Walking up from the inner node reaches the CompilationUnit root.
    let top = mul.ancestors().last().expect("an ancestor chain");
    assert_eq!(top.kind(), SyntaxKind::CompilationUnit);
    assert!(top.ptr_eq(&root), "ancestor walk returns to the same root");

    // Each node's absolute range indexes back into the exact source slice.
    for node in root.descendants() {
        let range = node.text_range();
        let slice = &src[range.start().to_u32() as usize..range.end().to_u32() as usize];
        assert_eq!(
            node.text(),
            slice,
            "node text equals its absolute source slice"
        );
    }
}

#[test]
fn red_tree_sibling_navigation_is_symmetric() {
    // `[a, b, c]` gives a list with three path-expr element children.
    let root = red_root("[a, b, c]");
    let list = first(&root, SyntaxKind::ListExpr).expect("a ListExpr");
    let elems = list.children();
    assert_eq!(elems.len(), 3, "three list elements, got {elems:?}");
    // next/prev sibling round-trips across the middle element.
    let mid = &elems[1];
    let next = mid.next_sibling().expect("a next sibling");
    assert!(next.ptr_eq(&elems[2]));
    let back = next.prev_sibling().expect("a prev sibling");
    assert!(back.ptr_eq(mid), "prev of next returns to the same node");
}

#[test]
fn parenthesized_versus_tuple_expression() {
    // A single parenthesized expression is a ParenExpr; a comma makes it a tuple.
    let paren = red_root("(a + b)");
    assert!(first(&paren, SyntaxKind::ParenExpr).is_some());
    assert!(first(&paren, SyntaxKind::TupleExpr).is_none());

    let tuple = red_root("(a, b)");
    assert!(first(&tuple, SyntaxKind::TupleExpr).is_some());
}

#[test]
fn if_and_match_expressions_parse() {
    let if_root = red_root("if cond { a } else { b }");
    assert!(first(&if_root, SyntaxKind::IfExpr).is_some());

    let match_root = red_root("match x { 1 => a, _ => b }");
    let m = first(&match_root, SyntaxKind::MatchExpr).expect("a MatchExpr");
    let arms = m
        .descendants()
        .into_iter()
        .filter(|n| n.kind() == SyntaxKind::MatchArm)
        .count();
    assert_eq!(arms, 2, "two match arms");
}

#[test]
fn losslessness_holds_over_a_corpus() {
    let corpus = [
        "a + b * c - d / e",
        "a && b || c ?? d",
        "x.y.z(1, name: 2)[3]?",
        "-a * !b + ~c",
        "Foo { a: 1, b, ..rest }",
        "match v { A => 1, B if c => 2, _ => 3 }",
        "|x, mut y: i32| x + y",
        "move || { a; b }",
        "a as u32 as i64",
        "..end",
        "start..",
        "start..=end",
    ];
    for src in corpus {
        let root = red_root(src);
        assert_eq!(root.text(), src, "round-trip {src:?}");
    }
}

#[test]
fn recovery_never_panics_and_round_trips() {
    let mut state: u64 = 0x9E3779B97F4A7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let alphabet: &[u8] = b"()[]{}.,:;+-*/%<>=!&|^~?abc0123 \n";
    for _ in 0..3000 {
        let len = (next() % 40) as usize;
        let s: String = (0..len)
            .map(|_| alphabet[(next() as usize) % alphabet.len()] as char)
            .collect();
        let parsed = parse(&tokenize(&s), &s);
        assert_eq!(parsed.root.kind(), SyntaxKind::CompilationUnit);
        assert_eq!(parsed.root.text(), s, "parser must round-trip {s:?}");
    }
}
