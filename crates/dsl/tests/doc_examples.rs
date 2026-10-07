//! Documentation examples (§152.1): every ```viso block of the specification
//! and the rationale is accepted by one parser entry, every ```viso-invalid
//! block is rejected by all of them, and no example leaves a `...` placeholder
//! outside a comment. Blocks of the lexical part only have to tokenize.

use viso_dsl::syntax::grammar::{Entry, parse_entry};
use viso_dsl::syntax::{SyntaxKind, Token, tokenize};

const SPEC: &str = include_str!("../../../Viso_DSL_1.0.md");
const RATIONALE: &str = include_str!("../../../Viso_DSL_Rationale.md");

/// The entries an example may be written against.
const ENTRIES: [Entry; 5] = [
    Entry::CompilationUnit,
    Entry::ComponentMembers,
    Entry::NodeMembers,
    Entry::BlockBody,
    Entry::Expr,
];

/// One fenced block: its info string, the line it opens on, its body, and
/// whether it sits in the lexical part of the specification.
struct Block<'a> {
    file: &'static str,
    line: usize,
    info: &'a str,
    body: String,
    lexical: bool,
}

fn blocks(file: &'static str, text: &'static str) -> Vec<Block<'static>> {
    let mut out = Vec::new();
    let mut open: Option<(usize, &str, String, bool)> = None;
    let mut part = "";
    for (n, line) in text.lines().enumerate() {
        if open.is_none() && line.starts_with("# ") {
            part = line;
        }
        let fence = line.trim_start();
        match &mut open {
            None if fence.starts_with("```") => {
                let info = fence.trim_start_matches('`').trim();
                open = Some((n + 1, info, String::new(), part.contains("词法规范")));
            }
            Some(_) if fence.starts_with("```") => {
                let (line, info, body, lexical) = open.take().unwrap();
                out.push(Block {
                    file,
                    line,
                    info,
                    body,
                    lexical,
                });
            }
            Some((_, _, body, _)) => {
                body.push_str(line);
                body.push('\n');
            }
            None => {}
        }
    }
    assert!(open.is_none(), "{file}: an unclosed code fence");
    out
}

fn all_blocks() -> Vec<Block<'static>> {
    let mut all = blocks("Viso_DSL_1.0.md", SPEC);
    all.extend(blocks("Viso_DSL_Rationale.md", RATIONALE));
    all
}

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
