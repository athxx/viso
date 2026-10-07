//! The `.vs` source formatter: a normalizing re-layout driven by the lossless
//! CST token stream.
//!
//! The formatter walks the parse tree's leaf tokens in source order, comments
//! kept, and re-emits them under layout rules keyed on token kinds, the node
//! each token sits in, and where the author broke lines:
//!
//! - A `{ }` the author wrote on one line stays on one line when it fits in
//!   [`MAX_WIDTH`] columns (`Text { text: label; }`); any other block opens a
//!   line after `{`, indents its body one level, and closes on its own line.
//!   An empty block is `{}`. An import's item braces hug their items
//!   (`import a::{B, C};`).
//! - A `;` ends its line in an expanded block; so does a `,` at an expanded
//!   block's own level (match arms, record fields written one per line).
//! - A `}` keeps a following `,` `;` `)` `]` `.` `?` or `else` on its line.
//! - A line break the author put inside a statement is kept (folded to one),
//!   the continuation indented one level deeper per open `(`/`[`.
//! - Blank lines between items and statements are kept, a run folded to one;
//!   none is kept after `{` or before `}`.
//! - A comment on the line of the code before it stays there; a line comment
//!   ends its line.
//! - Spacing: one space between tokens, except that `:` `;` `,` `.` `::` `?`
//!   `)` `]` hug what precedes them; `(` and `[` hug a name or a closing
//!   bracket before them (a call, an index); a prefix `-`/`!`, `.`, `::`, `@`
//!   and `(`/`[` hug what follows; ranges hug both bounds; closure pipes hug
//!   their parameters; generic angle brackets hug their contents and the name
//!   before them (`List<List<I64>>`).
//!
//! The output depends only on the tokens, their nodes and the line breaks, all
//! of which the output reproduces, so `format(format(x)) == format(x)`, and
//! re-parsing the formatted text yields the same significant tokens: the
//! formatter only ever changes whitespace.
//!
//! Cold-path tooling (architecture section 7.2): one format pass per editor
//! request, over a small document — `String` building throughout is the right
//! choice.

use viso_dsl::syntax::SyntaxNode;
use viso_dsl::syntax::grammar::parse;
use viso_dsl::{LexError, SyntaxKind, tokenize};

/// One indentation level: four spaces.
const INDENT: &str = "    ";

/// The widest a line holding a block kept on one line may be.
pub const MAX_WIDTH: usize = 100;

/// One significant or comment token of the source.
#[derive(Debug, Clone)]
struct Tok {
    kind: SyntaxKind,
    text: String,
    /// The kind of the node the token sits in.
    parent: SyntaxKind,
    /// The line breaks in the whitespace before it.
    breaks: usize,
    /// It is a generic list's `<` or `>`.
    angle: bool,
    /// It is a closure's opening `|`.
    open_pipe: bool,
    /// It ends an attribute (`@after(Movement)`), whose line break starts a
    /// new item rather than continuing one.
    ends_attribute: bool,
    /// Its line must end after it: a string or char left open runs to the
    /// end of its line.
    ends_line: bool,
}

impl Tok {
    fn is(&self, kind: SyntaxKind) -> bool {
        self.kind == kind
    }

    fn is_line_comment(&self) -> bool {
        matches!(
            self.kind,
            SyntaxKind::LineComment | SyntaxKind::DocComment | SyntaxKind::ModuleDocComment
        )
    }

    /// A prefix operator, which hugs its operand.
    fn is_prefix(&self) -> bool {
        matches!(self.kind, SyntaxKind::Minus | SyntaxKind::Bang)
            && matches!(
                self.parent,
                SyntaxKind::UnaryExpr | SyntaxKind::LiteralExpr | SyntaxKind::LiteralPattern
            )
    }

    /// A range operator, which hugs its bounds.
    fn is_range(&self) -> bool {
        matches!(self.kind, SyntaxKind::DotDot | SyntaxKind::DotDotEq)
            && matches!(
                self.parent,
                SyntaxKind::RangeExpr | SyntaxKind::RangePattern
            )
    }

    /// An import's item braces, which hug their items.
    fn hugs_inside(&self) -> bool {
        self.parent == SyntaxKind::ImportDecl
    }
}

/// Formats `.vs` source text, returning the normalized layout.
///
/// The text is tokenized and parsed (to obtain the lossless token stream in
/// source order with each token's node), then re-emitted under the layout
/// rules described on this module. Significant tokens are preserved exactly;
/// only whitespace changes.
pub fn format(source: &str) -> String {
    let lexed = tokenize(source);
    let parsed = parse(&lexed, source);
    // A token left open as the last one runs to the end of the source (a raw
    // string or block comment) or of its line (a string or char): trimming
    // after it would cut its text, a final newline would join it.
    let open_end = lexed
        .iter()
        .rev()
        .find(|t| t.kind != SyntaxKind::Eof)
        .is_some_and(|t| {
            matches!(
                t.error,
                Some(
                    LexError::UnterminatedRawString
                        | LexError::UnterminatedBlockComment
                        | LexError::UnterminatedString
                        | LexError::UnterminatedChar
                )
            )
        });
    let open_lines: Vec<u32> = lexed
        .iter()
        .filter(|t| {
            matches!(
                t.error,
                Some(LexError::UnterminatedString | LexError::UnterminatedChar)
            )
        })
        .map(|t| t.range.start().to_u32())
        .collect();

    let root = SyntaxNode::new_root(parsed.root.clone());
    let mut tokens: Vec<Tok> = Vec::new();
    let mut breaks = 0;
    let mut in_params = false;
    for el in root.descendants_with_tokens() {
        let Some(t) = el.as_token() else {
            continue;
        };
        let kind = t.kind();
        if kind == SyntaxKind::Whitespace {
            breaks += t.text().matches('\n').count();
            continue;
        }
        if kind == SyntaxKind::Eof {
            continue;
        }
        let parent = t.parent().kind();
        let open_pipe = kind == SyntaxKind::Pipe && parent == SyntaxKind::ClosureParams && {
            in_params = !in_params;
            in_params
        };
        tokens.push(Tok {
            kind,
            text: t.text(),
            parent,
            breaks: std::mem::take(&mut breaks),
            angle: is_generic_angle(kind, parent),
            open_pipe,
            ends_attribute: t.parent().ancestors().any(|node| {
                node.kind() == SyntaxKind::Attribute
                    && node.text_range().end() == t.text_range().end()
            }),
            ends_line: open_lines
                .binary_search(&t.text_range().start().to_u32())
                .is_ok(),
        });
    }

    let closes = matching_braces(&tokens);
    let mut printer = Printer::new(&tokens, &closes);
    printer.run();
    printer.finish(open_end)
}

/// The index of the `}` closing each `{`, by the `{`'s index.
fn matching_braces(tokens: &[Tok]) -> Vec<Option<usize>> {
    let mut closes = vec![None; tokens.len()];
    let mut open = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        match t.kind {
            SyntaxKind::LBrace => open.push(i),
            SyntaxKind::RBrace => {
                if let Some(at) = open.pop() {
                    closes[at] = Some(i);
                }
            }
            _ => {}
        }
    }
    closes
}

/// Whether `before` and `after` written with nothing between them would lex
/// as other tokens (`:` `:` as `::`, `/` `/` as a comment), so a space must
/// keep them apart.
fn joins(before: &str, before_kind: SyntaxKind, after: &str, after_kind: SyntaxKind) -> bool {
    let both = format!("{before}{after}");
    let kinds: Vec<SyntaxKind> = tokenize(&both)
        .iter()
        .map(|t| t.kind)
        .filter(|&k| k != SyntaxKind::Eof)
        .collect();
    kinds != [before_kind, after_kind]
}

/// Whether a token of `kind` under a `parent` node is a generic list's `<` or `>`.
fn is_generic_angle(kind: SyntaxKind, parent: SyntaxKind) -> bool {
    matches!(kind, SyntaxKind::Lt | SyntaxKind::Gt)
        && matches!(parent, SyntaxKind::GenericParams | SyntaxKind::GenericArgs)
}

/// Whether one space separates `prev` and `next` on a line.
fn spaced(prev: &Tok, next: &Tok) -> bool {
    use SyntaxKind as K;
    // Generic closers may touch: the parser splits a `>>` it closes on.
    if !(prev.angle && next.angle) && joins(&prev.text, prev.kind, &next.text, next.kind) {
        return true;
    }
    // A generic `<` hugs its name and its first argument; a generic `>` hugs
    // its last argument.
    if next.angle || (prev.angle && prev.is(K::Lt)) {
        return false;
    }
    if prev.is(K::LBrace) {
        return !next.is(K::RBrace) && !prev.hugs_inside();
    }
    if next.is(K::RBrace) {
        return !next.hugs_inside();
    }
    match next.kind {
        K::Colon
        | K::Semi
        | K::Comma
        | K::Dot
        | K::ColonColon
        | K::QuestionDot
        | K::RParen
        | K::RBracket => return false,
        K::Question if next.parent == K::TryExpr => return false,
        K::LParen | K::LBracket => {
            return !(matches!(
                prev.kind,
                K::Ident
                    | K::RawIdent
                    | K::RParen
                    | K::RBracket
                    | K::Question
                    | K::LParen
                    | K::LBracket
            ) || prev.angle);
        }
        K::Pipe if !next.open_pipe && next.parent == K::ClosureParams => {
            return prev.open_pipe;
        }
        _ => {}
    }
    if next.is_range() || prev.is_range() || prev.is_prefix() || prev.open_pipe {
        return false;
    }
    !matches!(
        prev.kind,
        K::LParen | K::LBracket | K::Dot | K::ColonColon | K::QuestionDot | K::At
    )
}

/// An expanded block being printed.
struct Frame {
    /// The open brackets when it opened: a `,` with no more open ends a line.
    groups: usize,
    /// The index of its `}`, if it has one.
    close: Option<usize>,
}

/// Accumulates formatted output, tracking indentation and owed breaks.
struct Printer<'t> {
    tokens: &'t [Tok],
    closes: &'t [Option<usize>],
    out: String,
    /// Expanded blocks open around the next token.
    frames: Vec<Frame>,
    /// Open `(`, `[` and generic `<`.
    groups: usize,
    /// The next token starts a line.
    pending_newline: bool,
    /// The index of the last token written.
    last: Option<usize>,
}

impl<'t> Printer<'t> {
    fn new(tokens: &'t [Tok], closes: &'t [Option<usize>]) -> Self {
        Printer {
            tokens,
            closes,
            out: String::new(),
            frames: Vec::new(),
            groups: 0,
            pending_newline: false,
            last: None,
        }
    }

    fn run(&mut self) {
        let mut i = 0;
        while i < self.tokens.len() {
            i = self.token(i);
        }
    }

    /// The column the output is at.
    fn column(&self) -> usize {
        let line = self.out.rsplit('\n').next().unwrap_or("");
        line.chars().count()
    }

    /// Emits token `i`, or the one-line block it opens, and returns the index
    /// of the next token to emit.
    fn token(&mut self, i: usize) -> usize {
        let tokens = self.tokens;
        let t = &tokens[i];
        let prev = self.last.map(|j| &tokens[j]);
        self.place(i, t, prev);

        if t.is(SyntaxKind::LBrace)
            && let Some(close) = self.closes[i]
            && let Some(flat) = self.flat(i, close)
            && self.column() + flat.chars().count() <= MAX_WIDTH
        {
            self.out.push_str(&flat);
            self.last = Some(close);
            self.after_close();
            return close + 1;
        }

        self.out.push_str(&t.text);
        self.last = Some(i);
        match t.kind {
            SyntaxKind::LBrace => {
                self.frames.push(Frame {
                    groups: self.groups,
                    close: self.closes[i],
                });
                self.pending_newline = true;
            }
            SyntaxKind::RBrace => self.after_close(),
            SyntaxKind::LParen | SyntaxKind::LBracket => self.groups += 1,
            SyntaxKind::Lt if t.angle => self.groups += 1,
            SyntaxKind::Gt if t.angle => self.groups = self.groups.saturating_sub(1),
            SyntaxKind::RParen | SyntaxKind::RBracket => {
                self.groups = self.groups.saturating_sub(1);
            }
            SyntaxKind::Semi => self.pending_newline = true,
            SyntaxKind::Comma
                if self
                    .frames
                    .last()
                    .is_some_and(|frame| frame.groups == self.groups) =>
            {
                self.pending_newline = true;
            }
            _ => {}
        }
        if t.is_line_comment() || t.ends_line {
            self.pending_newline = true;
        }
        i + 1
    }

    /// After a block closes: the next token starts a line, unless it keeps to
    /// the close's.
    fn after_close(&mut self) {
        self.pending_newline = true;
    }

    /// Breaks the line before `t` or spaces it from `prev`, and indents it.
    fn place(&mut self, i: usize, t: &Tok, prev: Option<&Tok>) {
        let Some(prev) = prev else {
            return;
        };
        if t.is(SyntaxKind::RBrace) && self.closes_frame(i) {
            self.frames.pop();
            if !prev.is(SyntaxKind::LBrace) {
                self.line(t, prev, 0);
                return;
            }
            // `{` `}` with nothing between, expanded only by a line break.
            self.pending_newline = false;
            return;
        }
        // A token that keeps to the line of the `}` before it.
        if prev.is(SyntaxKind::RBrace)
            && !self.ends_line_after(prev)
            && matches!(
                t.kind,
                SyntaxKind::Comma
                    | SyntaxKind::Semi
                    | SyntaxKind::RParen
                    | SyntaxKind::RBracket
                    | SyntaxKind::Dot
                    | SyntaxKind::Question
                    | SyntaxKind::QuestionDot
                    | SyntaxKind::ElseKw
            )
        {
            self.pending_newline = false;
            if spaced(prev, t) {
                self.out.push(' ');
            }
            return;
        }
        // A comment written after code on its line stays there.
        if t.breaks == 0
            && matches!(
                t.kind,
                SyntaxKind::LineComment | SyntaxKind::DocComment | SyntaxKind::BlockComment
            )
            && !self.ends_line_after(prev)
        {
            self.pending_newline = false;
        }
        if self.pending_newline {
            self.line(t, prev, 0);
            return;
        }
        let hugs = matches!(
            t.kind,
            SyntaxKind::Comma | SyntaxKind::Semi | SyntaxKind::Colon
        );
        if t.breaks > 0 && prev.ends_attribute {
            self.line(t, prev, 0);
            return;
        }
        if t.breaks > 0 && !hugs {
            let closing = matches!(t.kind, SyntaxKind::RParen | SyntaxKind::RBracket);
            let open = self.open_groups();
            let extra = if closing {
                open.saturating_sub(1)
            } else {
                open.max(1)
            };
            self.line(t, prev, extra);
            return;
        }
        if spaced(prev, t) {
            self.out.push(' ');
        }
    }

    /// Whether the line must end after `prev` whatever follows.
    fn ends_line_after(&self, prev: &Tok) -> bool {
        prev.is_line_comment() || prev.ends_line
    }

    /// Whether the `}` at `i` closes the innermost expanded block.
    fn closes_frame(&self, i: usize) -> bool {
        self.frames
            .last()
            .is_some_and(|frame| frame.close == Some(i))
    }

    /// The brackets open within the innermost expanded block.
    fn open_groups(&self) -> usize {
        let base = self.frames.last().map_or(0, |frame| frame.groups);
        self.groups.saturating_sub(base)
    }

    /// Starts `t` on a fresh line indented `extra` levels past its block,
    /// after a blank line where the author left one.
    fn line(&mut self, t: &Tok, prev: &Tok, extra: usize) {
        self.pending_newline = false;
        self.out.push('\n');
        if t.breaks >= 2 && !prev.is(SyntaxKind::LBrace) && !t.is(SyntaxKind::RBrace) {
            self.out.push('\n');
        }
        for _ in 0..self.frames.len() + extra {
            self.out.push_str(INDENT);
        }
    }

    /// Tokens `open..=close`, a block, on one line, if the author wrote them
    /// on one and nothing in them must end a line.
    fn flat(&self, open: usize, close: usize) -> Option<String> {
        let span = &self.tokens[open..=close];
        if span[1..].iter().any(|t| t.breaks > 0)
            || span.iter().any(|t| t.is_line_comment() || t.ends_line)
        {
            return None;
        }
        let mut out = String::new();
        for (k, t) in span.iter().enumerate() {
            if k > 0 && spaced(&span[k - 1], t) {
                out.push(' ');
            }
            out.push_str(&t.text);
        }
        Some(out)
    }

    /// Finishes the document: the output ends with exactly one trailing newline
    /// (and none if the document is empty).
    fn finish(mut self, open_end: bool) -> String {
        if open_end {
            return self.out;
        }
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
        assert_eq!(
            format("component C{\nstate count=0;}"),
            "component C {\n    state count = 0;\n}\n"
        );
        // A block written on one line stays on one line.
        assert_eq!(
            format("component C{state count=0;}"),
            "component C { state count = 0; }\n"
        );
    }

    #[test]
    fn a_long_one_line_block_expands() {
        let long = format!("Text {{ text: \"{}\"; }}", "x".repeat(MAX_WIDTH));
        assert_eq!(
            format(&long),
            format!("Text {{\n    text: \"{}\";\n}}\n", "x".repeat(MAX_WIDTH))
        );
    }

    #[test]
    fn format_preserves_comments() {
        let src = "component C {\n// a leading note\nstate count = 0; // a trailing note\n}\n";
        let formatted = format(src);
        assert_eq!(
            formatted,
            "component C {\n    // a leading note\n    state count = 0; // a trailing note\n}\n"
        );
        assert_eq!(format(&formatted), formatted);
    }

    #[test]
    fn format_folds_surplus_blank_lines() {
        let src =
            "component A {}\n\n\n\ncomponent B {\n\n    state a = 1;\n\n\n    state b = 2;\n\n}\n";
        assert_eq!(
            format(src),
            "component A {}\n\ncomponent B {\n    state a = 1;\n\n    state b = 2;\n}\n"
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
    fn format_hugs_generic_angles_but_spaces_comparisons() {
        assert_eq!(
            format("type M = List < List < I64 > >;"),
            "type M = List<List<I64>>;\n"
        );
        assert_eq!(
            format("fn f < T > (x: Map<String,T>) -> Bool {\n return a<b; }"),
            "fn f<T>(x: Map<String, T>) -> Bool {\n    return a < b;\n}\n"
        );
    }

    #[test]
    fn closing_braces_keep_what_follows_them() {
        assert_eq!(
            format("fn f() {\nif a {\nx;\n}\nelse {\ny;\n}\n}"),
            "fn f() {\n    if a {\n        x;\n    } else {\n        y;\n    }\n}\n"
        );
        assert_eq!(
            format("fn f() {\nmatch x {\n1 => {\na;\n}\n, _ => {\nb;\n}\n,\n}\n}"),
            "fn f() {\n    match x {\n        1 => {\n            a;\n        },\n        _ => {\n            b;\n        },\n    }\n}\n"
        );
    }

    #[test]
    fn operators_brackets_and_imports_space_as_written_in_the_spec() {
        assert_eq!(
            format("import viso::game::{ A,B };"),
            "import viso::game::{A, B};\n"
        );
        assert_eq!(
            format("fn f() { let xs = [1]; let y = -x + !b; let r = 0..n; let g = |a, b| a; }"),
            "fn f() { let xs = [1]; let y = -x + !b; let r = 0..n; let g = |a, b| a; }\n"
        );
        assert_eq!(
            format("fn f() { g (x)[0]; h(( a )); }"),
            "fn f() { g(x)[0]; h((a)); }\n"
        );
    }

    #[test]
    fn a_line_break_inside_a_statement_is_kept() {
        assert_eq!(
            format("fn f() {\nlet x = call(a,\nb);\nlet y = a\n+ b;\n}"),
            "fn f() {\n    let x = call(a,\n        b);\n    let y = a\n        + b;\n}\n"
        );
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
