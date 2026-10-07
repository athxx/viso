//! Formatter fuzz (§152.1): random and mutated sources never make the
//! formatter panic, and over every documentation example that parses as a
//! compilation unit, the formatted text parses to the same tree — node kinds
//! and significant tokens — and formats to itself.

use viso_dsl::syntax::SyntaxNode;
use viso_dsl::syntax::grammar::{Entry, parse_entry};
use viso_dsl::{SyntaxKind, tokenize};
use viso_lsp::format::format;

const SPEC: &str = include_str!("../../../Viso_DSL_1.0.md");
const RATIONALE: &str = include_str!("../../../Viso_DSL_Rationale.md");

/// The ```viso blocks of the specification and the rationale.
fn examples() -> Vec<String> {
    let mut out = Vec::new();
    for text in [SPEC, RATIONALE] {
        let mut open: Option<String> = None;
        for line in text.lines() {
            let fence = line.trim_start();
            match &mut open {
                None if fence == "```viso" => open = Some(String::new()),
                Some(_) if fence.starts_with("```") => out.push(open.take().unwrap()),
                Some(body) => {
                    body.push_str(line);
                    body.push('\n');
                }
                None => {}
            }
        }
    }
    out
}

/// The tree's shape with layout erased: each node's kind with its children
/// nested, each significant token's kind and text.
fn shape(src: &str) -> String {
    fn walk(node: &SyntaxNode, out: &mut String) {
        out.push_str(&format!("{:?}(", node.kind()));
        for el in node.children_with_tokens() {
            if let Some(n) = el.as_node() {
                walk(n, out);
            } else if let Some(t) = el.as_token()
                && !t.kind().is_trivia()
                && t.kind() != SyntaxKind::Eof
            {
                out.push_str(&format!("{:?}{:?} ", t.kind(), t.text()));
            }
        }
        out.push(')');
    }
    let parse = parse_entry(&tokenize(src), src, Entry::CompilationUnit);
    let mut out = String::new();
    walk(&SyntaxNode::new_root(parse.root), &mut out);
    out
}

fn parses_clean(src: &str) -> bool {
    let tokens = tokenize(src);
    tokens.iter().all(|t| t.error.is_none())
        && parse_entry(&tokens, src, Entry::CompilationUnit)
            .errors
            .is_empty()
}

#[test]
fn formatting_keeps_the_tree_of_every_example() {
    let mut checked = 0;
    let mut failures = Vec::new();
    for src in examples().iter().filter(|s| parses_clean(s)) {
        checked += 1;
        let once = format(src);
        if shape(&once) != shape(src) {
            failures.push(format!(
                "the tree changed:\n{src}\n--- formatted ---\n{once}"
            ));
        }
        if format(&once) != once {
            failures.push(format!("not idempotent:\n{src}"));
        }
    }
    assert!(checked > 40, "only {checked} examples parse as a unit");
    assert!(failures.is_empty(), "{}", failures.join("\n=====\n"));
}

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

/// Tokens and fragments random sources are drawn from, broken ones included.
const ALPHABET: &[&str] = &[
    "component",
    "state",
    "view",
    "fn",
    "if",
    "else",
    "match",
    "for",
    "in",
    "key",
    "let",
    "on",
    "bind",
    "<=>",
    "=>",
    "x",
    "Text",
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    "<",
    ">",
    ">>",
    ";",
    ":",
    ",",
    ".",
    "::",
    "=",
    "+",
    "-",
    "%",
    "50%",
    "1em",
    "1e5",
    "\"s\"",
    "\"open",
    "'c'",
    "r#\"",
    "/*",
    "*/",
    "// c\n",
    "\n",
    " ",
    "\t",
    "#fff",
    "é",
    "\u{430}",
    "\r",
    "\0",
    "@",
    "|",
    "||",
    "?",
    "?.",
    "..",
    "..=",
];

#[test]
fn random_and_mutated_sources_never_make_the_formatter_panic() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for _ in 0..3000 {
        let len = rng.below(60);
        let src: String = (0..len)
            .map(|_| ALPHABET[rng.below(ALPHABET.len())])
            .collect();
        let once = format(&src);
        // Even broken input keeps its significant tokens.
        assert_eq!(
            significant(&once),
            significant(&src),
            "{src:?} formatted to {once:?}"
        );
    }
    let corpus = examples();
    for _ in 0..1500 {
        let mut src = corpus[rng.below(corpus.len())].clone();
        for _ in 0..1 + rng.below(4) {
            let mut at = rng.below(src.len() + 1);
            while !src.is_char_boundary(at) {
                at -= 1;
            }
            let mut end = (at + rng.below(8)).min(src.len());
            while !src.is_char_boundary(end) {
                end += 1;
            }
            src.replace_range(at..end, ALPHABET[rng.below(ALPHABET.len())]);
        }
        let once = format(&src);
        assert_eq!(significant(&once), significant(&src), "{src:?}");
    }
}

/// The significant tokens of the parse of `src`, kind and text.
fn significant(src: &str) -> Vec<(SyntaxKind, String)> {
    let parse = parse_entry(&tokenize(src), src, Entry::CompilationUnit);
    SyntaxNode::new_root(parse.root)
        .descendants_with_tokens()
        .into_iter()
        .filter_map(|el| el.as_token().map(|t| (t.kind(), t.text())))
        .filter(|(kind, _)| !kind.is_trivia() && *kind != SyntaxKind::Eof)
        .collect()
}
