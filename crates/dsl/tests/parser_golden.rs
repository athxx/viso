//! Parser golden: for every Core production, one positive case that parses with
//! no diagnostics and one negative case that yields exactly the expected stable
//! codes. Every case must also round-trip byte-for-byte, so recovery never drops
//! or duplicates source.

use viso_dsl::syntax::grammar::parse_entry;
use viso_dsl::syntax::tokenize;

#[path = "golden/parser_cases.rs"]
mod cases;

use cases::CASES;

#[test]
fn every_production_has_its_golden_diagnostics() {
    let mut failures = Vec::new();
    for case in CASES {
        let parse = parse_entry(&tokenize(case.src), case.src, case.entry);
        if parse.root.text() != case.src {
            failures.push(format!(
                "{}: not lossless for {:?}",
                case.production, case.src
            ));
        }
        let mut got: Vec<&str> = parse.errors.iter().map(|e| e.code).collect();
        got.sort_unstable();
        got.dedup();
        if got != case.codes {
            failures.push(format!(
                "{}: {:?} expected {:?}, got {:?}",
                case.production, case.src, case.codes, got
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "golden mismatches:\n{}",
        failures.join("\n")
    );
}

#[test]
fn every_production_has_a_positive_and_a_negative_case() {
    // Productions listed here are pinned by both directions; the others above
    // are positive-only variants of a production that already has a negative.
    let paired = [
        "ImportDecl",
        "ComponentDecl",
        "InputDecl",
        "StateDecl",
        "ComputedDecl",
        "EventDecl",
        "SlotDecl",
        "ActionDecl",
        "ViewDecl",
        "FnDecl",
        "RecordDecl",
        "EnumDecl",
        "TypeAlias",
        "ConstDecl",
        "GenericParams",
        "GenericArgs",
        "LetStmt",
        "AssignStmt",
        "IfStmt",
        "ForStmt",
        "EmitStmt",
        "BinaryExpr",
        "ComparisonExpr",
        "EqualityExpr",
        "RangeExpr",
        "CallExpr",
        "Turbofish",
        "IndexExpr",
        "RecordExpr",
        "ListExpr",
        "MatchExpr",
        "IdentPattern",
        "TuplePattern",
        "ConstructorPattern",
        "AnonymousNode",
        "NamedNode",
        "EventHandler",
        "ViewFor",
    ];
    for production in paired {
        let of = |ok: bool| {
            CASES
                .iter()
                .any(|c| c.production == production && c.codes.is_empty() == ok)
        };
        assert!(of(true), "{production} lacks a positive case");
        assert!(of(false), "{production} lacks a negative case");
    }
}
