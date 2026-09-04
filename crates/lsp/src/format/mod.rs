//! The `.vs` source formatter: a normalizing re-layout driven by the lossless
//! CST token stream.
//!
//! The formatter does **not** consult the grammar. It walks the parse tree's flat
//! leaf-token stream in source order (the same stream the lexer produced, with
//! `Whitespace` trivia dropped and comments kept), then re-emits it under a fixed
//! set of layout rules keyed only on token kinds and brace depth:
//!
//! - Indentation is a level per enclosing `{ }`: `{` opens a level (its body is
//!   indented one deeper), `}` closes it and dedents to the enclosing level.
//! - A statement/binding terminator `;` ends the current line; a block open `{`
//!   ends the line after it; a block close `}` starts on its own line.
//! - Property/type binding punctuation is tightened per DSL style
//!   (architecture section 21.5.2): no space before `:` `;` `,`, one space after.
//! - Surplus blank lines are folded (a run of blank lines collapses to at most
//!   one), and comments are preserved in place — a line comment ends its line, a
//!   block comment is spaced like an ordinary token.
//!
//! Because the rules depend only on token kinds, the output is stable:
//! `format(format(x)) == format(x)` (the idempotency anchor the tests assert),
//! and re-parsing the formatted text yields the same non-trivia token stream
//! (the round-trip anchor — the formatter never changes a semantic token, only
//! trivia and layout).
//!
//! Cold-path tooling (architecture section 7.2): one format pass per editor
//! request, over a small document — `String` building throughout is the right
//! choice.

use viso_dsl::syntax::SyntaxNode;
use viso_dsl::syntax::grammar::parse;
use viso_dsl::{SyntaxKind, tokenize};

/// One indentation level: four spaces.
const INDENT: &str = "    ";

/// Formats `.vs` source text, returning the normalized layout.
///
/// The text is tokenized and parsed (to obtain the lossless token stream in
/// source order), then re-emitted under the layout rules described on this
/// module. Semantic tokens are preserved exactly; only whitespace and the
/// placement of comments change.
pub fn format(source: &str) -> String {
    let parsed = parse(&tokenize(source), source);
    let root = SyntaxNode::new_root(parsed.root.clone());

    // The flat leaf-token stream in source order, whitespace dropped, comments
    // kept. Each entry is (kind, text).
    let tokens: Vec<(SyntaxKind, String)> = root
        .descendants_with_tokens()
        .into_iter()
        .filter_map(|el| el.as_token().map(|t| (t.kind(), t.text())))
        .filter(|(kind, _)| *kind != SyntaxKind::Whitespace)
        .collect();

    let mut printer = Printer::new();
    for i in 0..tokens.len() {
        let (kind, text) = &tokens[i];
        let prev = i.checked_sub(1usize).map(|j| tokens[j].0);
        let next = tokens.get(i + 1).map(|(k, _)| *k);
        printer.emit(*kind, text, prev, next);
    }
    printer.finish()
}

/// Accumulates formatted output, tracking indentation depth and pending
/// line/blank-line breaks so the layout rules can be expressed per token.
struct Printer {
    out: String,
    depth: usize,
    /// Whether the current output position is at the start of a fresh line (so the
    /// next visible token must first be indented).
    at_line_start: bool,
    /// A newline is owed before the next token (a statement/block boundary).
    pending_newline: bool,
    /// A blank line is owed before the next token (folded surplus blank lines).
    pending_blank: bool,
}

impl Printer {
    fn new() -> Self {
        Printer {
            out: String::new(),
            depth: 0,
            at_line_start: true,
            pending_newline: false,
            pending_blank: false,
        }
    }

    /// Emits one token, applying spacing relative to `prev` (the previous
    /// non-whitespace token kind, if any) and `next` (the following kind, used only
    /// to keep an empty `{}` block inline).
    fn emit(
        &mut self,
        kind: SyntaxKind,
        text: &str,
        prev: Option<SyntaxKind>,
        next: Option<SyntaxKind>,
    ) {
        if kind == SyntaxKind::RBrace {
            // Close brace dedents. It starts its own line *unless* the block is empty
            // (the immediately preceding token was its own `{`), in which case the
            // pending newline from that `{` is cancelled to keep `{}` inline.
            self.depth = self.depth.saturating_sub(1);
            if prev == Some(SyntaxKind::LBrace) {
                self.pending_newline = false;
            } else {
                self.newline();
            }
        }

        self.flush_breaks();

        if !self.at_line_start {
            if self.wants_space_before(kind, prev) {
                self.out.push(' ');
            }
        } else {
            self.push_indent();
        }

        self.out.push_str(text);
        self.at_line_start = false;

        match kind {
            SyntaxKind::LBrace if next == Some(SyntaxKind::RBrace) => {
                // Empty block: still open a depth level (the matching `}` closes it),
                // but do not break — the `}` follows immediately.
                self.depth += 1;
            }
            SyntaxKind::LBrace => {
                self.depth += 1;
                self.pending_newline = true;
            }
            SyntaxKind::RBrace | SyntaxKind::Semi => {
                // A block close and a statement/binding terminator both end their
                // line, so the next token starts a new one.
                self.pending_newline = true;
            }
            SyntaxKind::LineComment | SyntaxKind::DocComment | SyntaxKind::ModuleDocComment => {
                // A line comment runs to end of line; the next token must break.
                self.pending_newline = true;
            }
            _ => {}
        }
    }

    /// Whether a space is required between `prev` and the token about to be emitted.
    ///
    /// The default is one space (token separation); the exceptions tighten the DSL
    /// binding/call punctuation.
    fn wants_space_before(&self, kind: SyntaxKind, prev: Option<SyntaxKind>) -> bool {
        let Some(prev) = prev else {
            return false;
        };
        // No space *before* these: they hug the preceding token.
        if matches!(
            kind,
            SyntaxKind::Colon
                | SyntaxKind::Semi
                | SyntaxKind::Comma
                | SyntaxKind::LParen
                | SyntaxKind::RParen
                | SyntaxKind::LBracket
                | SyntaxKind::RBracket
                | SyntaxKind::Dot
                | SyntaxKind::ColonColon
        ) {
            // `(` after a name is a call/param list (hug); `(` after a keyword or
            // operator wants a space handled by the after-rules below, but hugging is
            // the safe DSL default here.
            return false;
        }
        // No space *after* these: the preceding token hugs the next.
        if matches!(
            prev,
            SyntaxKind::LParen
                | SyntaxKind::LBracket
                | SyntaxKind::LBrace
                | SyntaxKind::Dot
                | SyntaxKind::ColonColon
                | SyntaxKind::At
        ) {
            return false;
        }
        true
    }

    /// Requests a newline before the next token (unless one is already pending).
    fn newline(&mut self) {
        self.pending_newline = true;
    }

    /// Writes any owed newline / blank line, then leaves the printer at a fresh line
    /// start (so the caller pushes indentation before the token).
    fn flush_breaks(&mut self) {
        if self.pending_newline || self.pending_blank {
            // Only emit a break if we have already written something; leading breaks
            // would produce a blank first line.
            if !self.out.is_empty() {
                self.out.push('\n');
                if self.pending_blank {
                    self.out.push('\n');
                }
            }
            self.at_line_start = true;
        }
        self.pending_newline = false;
        self.pending_blank = false;
    }

    fn push_indent(&mut self) {
        for _ in 0..self.depth {
            self.out.push_str(INDENT);
        }
    }

    /// Finishes the document: the output ends with exactly one trailing newline
    /// (and none if the document is empty).
    fn finish(mut self) -> String {
        // Drop any trailing whitespace/newlines we may have accumulated, then add a
        // single terminating newline if there is any content.
        while self.out.ends_with('\n') || self.out.ends_with(' ') {
            self.out.pop();
        }
        if !self.out.is_empty() {
            self.out.push('\n');
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_dsl::{SyntaxKind, tokenize};

    /// The non-trivia token kinds of a source, in order — the semantic token stream
    /// the formatter must preserve.
    fn semantic_kinds(src: &str) -> Vec<SyntaxKind> {
        tokenize(src)
            .into_iter()
            .filter(|t| !t.is_trivia() && t.kind != SyntaxKind::Eof)
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn format_is_idempotent() {
        let samples = [
            "component C {\n  state count = 0;\n  computed d = count;\n}\n",
            "component   Counter{state  count=0;computed doubled=count;}",
            "record P { x: Int; y: Int; }",
            "component C {\n  view {\n    Text { text: label; color: c; }\n  }\n}\n",
        ];
        for src in samples {
            let once = format(src);
            let twice = format(&once);
            assert_eq!(once, twice, "format must be idempotent for:\n{src}");
        }
    }

    #[test]
    fn format_preserves_semantic_tokens() {
        let src = "component   Counter{state  count=0;computed doubled=count;}";
        let formatted = format(src);
        assert_eq!(
            semantic_kinds(src),
            semantic_kinds(&formatted),
            "the semantic token stream must be unchanged by formatting"
        );
    }

    #[test]
    fn format_indents_blocks_and_terminates_bindings() {
        let src = "component C{state count=0;}";
        let formatted = format(src);
        let expected = "component C {\n    state count = 0;\n}\n";
        assert_eq!(formatted, expected);
    }

    #[test]
    fn format_preserves_comments() {
        let src = "component C {\n// a leading note\nstate count = 0;\n}\n";
        let formatted = format(src);
        assert!(
            formatted.contains("// a leading note"),
            "line comment must survive formatting:\n{formatted}"
        );
        // And it is still idempotent with the comment present.
        assert_eq!(format(&formatted), formatted);
    }

    #[test]
    fn format_folds_surplus_blank_lines() {
        let src = "component A {}\n\n\n\ncomponent B {}\n";
        let formatted = format(src);
        assert!(
            !formatted.contains("\n\n\n"),
            "runs of blank lines must fold to at most one:\n{formatted:?}"
        );
    }

    #[test]
    fn format_tightens_binding_punctuation() {
        let src = "record P{x : Int ; y : Int ;}";
        let formatted = format(src);
        assert!(formatted.contains("x: Int;"), "got:\n{formatted}");
        assert!(formatted.contains("y: Int;"), "got:\n{formatted}");
    }

    #[test]
    fn format_of_empty_source_is_empty() {
        assert_eq!(format(""), "");
        assert_eq!(format("   \n  \n"), "");
    }

    #[test]
    fn format_ends_with_single_newline() {
        let formatted = format("component C {}");
        assert!(formatted.ends_with("}\n"));
        assert!(!formatted.ends_with("\n\n"));
    }
}
