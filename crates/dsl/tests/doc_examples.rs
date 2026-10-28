//! Documentation examples: every ```viso block of the specification
//! and the rationale is accepted by one parser entry, every ```viso-invalid
//! block is rejected by all of them, and no example leaves a `...` placeholder
//! outside a comment. Blocks of the lexical part only have to tokenize.

use viso_dsl::syntax::grammar::{Entry, parse_entry};
use viso_dsl::syntax::{SyntaxKind, Token, tokenize};

#[path = "support/doc_blocks.rs"]
mod doc_blocks;

use doc_blocks::all_blocks;

/// The entries an example may be written against.
const ENTRIES: [Entry; 5] = [
    Entry::CompilationUnit,
    Entry::ComponentMembers,
    Entry::NodeMembers,
    Entry::BlockBody,
    Entry::Expr,
];

/// The parser diagnostics `src` gets under `entry`, lexical errors included.
fn errors(tokens: &[Token], src: &str, entry: Entry) -> Vec<String> {
    let mut out: Vec<String> = tokens
        .iter()
        .filter_map(|t| t.error.map(|e| format!("{e:?} at {:?}", t.range)))
        .collect();
    out.extend(
        parse_entry(tokens, src, entry)
            .errors
            .iter()
            .map(|d| format!("{} {} at {:?}", d.code, d.message, d.primary)),
    );
    out
}

/// A `...` written as code: three adjacent dots outside comments and strings.
fn has_placeholder(tokens: &[Token]) -> bool {
    let code: Vec<&Token> = tokens.iter().filter(|t| !t.kind.is_trivia()).collect();
    code.windows(2).any(|w| {
        let adjacent = w[0].range.end() == w[1].range.start();
        adjacent
            && matches!(
                (w[0].kind, w[1].kind),
                (SyntaxKind::DotDot, SyntaxKind::Dot) | (SyntaxKind::Dot, SyntaxKind::DotDot)
            )
    })
}

#[test]
fn every_viso_example_parses_under_one_entry() {
    let mut failures = Vec::new();
    let mut checked = 0;
    for block in all_blocks().iter().filter(|b| b.info == "viso") {
        checked += 1;
        let src = &block.body;
        let tokens = tokenize(src);
        let at = format!("{}:{}", block.file, block.line);
        if has_placeholder(&tokens) {
            failures.push(format!("{at}: a `...` placeholder outside a comment"));
        }
        if block.lexical {
            let lexical: Vec<_> = tokens.iter().filter_map(|t| t.error).collect();
            if !lexical.is_empty() {
                failures.push(format!("{at}: does not tokenize: {lexical:?}"));
            }
            continue;
        }
        let attempts: Vec<_> = ENTRIES.iter().map(|&e| errors(&tokens, src, e)).collect();
        if attempts.iter().all(|errors| !errors.is_empty()) {
            let best = attempts.iter().min_by_key(|e| e.len()).unwrap();
            failures.push(format!(
                "{at}: no entry accepts it; fewest errors: {best:?}"
            ));
        }
    }
    assert!(checked > 100, "only {checked} examples found");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_invalid_example_is_rejected_by_every_entry() {
    let mut failures = Vec::new();
    let mut checked = 0;
    for block in all_blocks().iter().filter(|b| b.info == "viso-invalid") {
        checked += 1;
        let tokens = tokenize(&block.body);
        for entry in ENTRIES {
            if errors(&tokens, &block.body, entry).is_empty() {
                failures.push(format!(
                    "{}:{}: accepted as {entry:?}",
                    block.file, block.line
                ));
            }
        }
    }
    assert!(checked > 0);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
