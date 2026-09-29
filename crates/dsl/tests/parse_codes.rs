//! Pins every parse-error code: each [`ParseErrorKind`] maps to its stable
//! Appendix C code, the spec's Appendix C table has a row for it, and a real
//! source snippet triggers it through the typed grammar.

use viso_dsl::syntax::grammar::{Entry, parse_entry};
use viso_dsl::syntax::{ParseErrorKind, tokenize};

/// The Viso DSL specification, whose Appendix C is the code registry.
const SPEC: &str = include_str!("../../../Viso_DSL_1.0.md");

/// Every kind, its frozen code, and an entry/snippet that produces it.
const PINNED: &[(ParseErrorKind, &str, Entry, &str)] = &[
    (
        ParseErrorKind::UnclosedDelimiter,
        "E1401",
        Entry::Expr,
        "f(1",
    ),
    (
        ParseErrorKind::UnmatchedCloser,
        "E1402",
        Entry::CompilationUnit,
        "}",
    ),
    (
        ParseErrorKind::UnexpectedTokens,
        "E1403",
        Entry::CompilationUnit,
        "component C { % }",
    ),
    (
        ParseErrorKind::MissingToken,
        "E1404",
        Entry::CompilationUnit,
        "type Id I64;",
    ),
    (ParseErrorKind::ExpectedExpr, "E1405", Entry::Expr, "a +"),
    (
        ParseErrorKind::NonAssocChain,
        "E2802",
        Entry::Expr,
        "a < b < c",
    ),
    (
        ParseErrorKind::NonAssocRange,
        "E2802",
        Entry::Expr,
        "a .. b .. c",
    ),
    (
        ParseErrorKind::RecordExprInHead,
        "E2801",
        Entry::Expr,
        "if P { x: 1 } { }",
    ),
    (
        ParseErrorKind::ChildReserved,
        "E3001",
        Entry::ViewFragment,
        "child Text { }",
    ),
    (
        ParseErrorKind::HandlerNotArrow,
        "E3201",
        Entry::ViewFragment,
        "B { on click => a(); }",
    ),
    (
        ParseErrorKind::ForMissingKey,
        "E3401",
        Entry::ViewFragment,
        "for i in xs { T { } }",
    ),
    (
        ParseErrorKind::PreserveNotLiteral,
        "E3301",
        Entry::ViewFragment,
        "if a preserve name { T { } }",
    ),
    (
        ParseErrorKind::ReservedIdent,
        "E1301",
        Entry::CompilationUnit,
        "fn f() { let fn = 1; }",
    ),
    (
        ParseErrorKind::GenericWithoutTurbofish,
        "E2004",
        Entry::Expr,
        "make<Foo>(x)",
    ),
    (
        ParseErrorKind::ConstArgWithoutConst,
        "E2004",
        Entry::Expr,
        "make::<Foo, 4>(x)",
    ),
];

#[test]
fn every_kind_keeps_its_frozen_code() {
    for &(kind, code, _, _) in PINNED {
        assert_eq!(kind.code(), code, "{kind:?} changed its stable code");
    }
}

#[test]
fn every_code_has_an_appendix_c_row() {
    let appendix = &SPEC[SPEC.find("# 附录 C").expect("Appendix C heading")..];
    for &(kind, code, _, _) in PINNED {
        assert!(
            appendix.contains(&format!("| {code} ")),
            "{kind:?} ({code}) has no Appendix C row"
        );
    }
}

#[test]
fn every_kind_is_reachable_from_source() {
    for &(kind, code, entry, src) in PINNED {
        let parse = parse_entry(&tokenize(src), src, entry);
        assert!(
            parse.errors.iter().any(|e| e.code == code),
            "{kind:?}: {src:?} did not report {code}, got {:?}",
            parse.errors.iter().map(|e| e.code).collect::<Vec<_>>()
        );
    }
}
