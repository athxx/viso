//! The formatter over the parser golden corpus: for every case, formatting is
//! idempotent, keeps the semantic token stream, and leaves the parse's diagnostic
//! codes unchanged, broken input included.

use viso_dsl::syntax::SyntaxNode;
use viso_dsl::syntax::grammar::{Entry, parse_entry};
use viso_dsl::{SyntaxKind, tokenize};
use viso_lsp::format::format;

#[path = "../../dsl/tests/golden/parser_cases.rs"]
mod cases;

use cases::CASES;

/// The non-trivia leaf tokens of `src` parsed as `entry`, kind and text, in order.
/// Leaves rather than lexer tokens, so a `>>` the parser splits into two generic
/// `>` compares equal to the two `>` a formatter may print.
fn semantic_tokens(src: &str, entry: Entry) -> Vec<(SyntaxKind, String)> {
    let parse = parse_entry(&tokenize(src), src, entry);
    SyntaxNode::new_root(parse.root.clone())
        .descendants_with_tokens()
        .into_iter()
        .filter_map(|el| el.as_token().map(|t| (t.kind(), t.text())))
        .filter(|(kind, _)| !kind.is_trivia() && *kind != SyntaxKind::Eof)
        .collect()
}

#[test]
fn formatting_every_golden_is_stable_and_changes_only_layout() {
    let mut failures = Vec::new();
    for case in CASES {
        let once = format(case.src);
        let twice = format(&once);
        if twice != once {
            failures.push(format!(
                "{}: not idempotent for {:?}\n  once:  {once:?}\n  twice: {twice:?}",
                case.production, case.src
            ));
        }
        if semantic_tokens(&once, case.entry) != semantic_tokens(case.src, case.entry) {
            failures.push(format!(
                "{}: tokens changed for {:?}\n  formatted: {once:?}",
                case.production, case.src
            ));
        }
        let mut codes: Vec<&str> = parse_entry(&tokenize(&once), &once, case.entry)
            .errors
            .iter()
            .map(|e| e.code)
            .collect();
        codes.sort_unstable();
        codes.dedup();
        if codes != case.codes {
            failures.push(format!(
                "{}: formatting {:?} changed its codes to {codes:?}\n  formatted: {once:?}",
                case.production, case.src
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} goldens:\n{}",
        failures.len(),
        CASES.len(),
        failures.join("\n")
    );
}
