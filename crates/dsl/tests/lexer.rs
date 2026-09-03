//! Positive lexing, disambiguation, malformed-input recovery, losslessness,
//! incremental re-lex, and a never-panic fuzz smoke test for the `.vs` lexer.

use viso_dsl::syntax::{
    Edit, LexError, LineIndex, SyntaxKind, TextRange, TextSize, flat_tree, parse, reparse_tokens,
    tokenize,
};

/// The non-trivia token kinds of `src`, dropping whitespace/comments and the
/// trailing `Eof`, for compact assertions.
fn significant_kinds(src: &str) -> Vec<SyntaxKind> {
    tokenize(src)
        .into_iter()
        .filter(|t| !t.kind.is_trivia() && t.kind != SyntaxKind::Eof)
        .map(|t| t.kind)
        .collect()
}

/// The (kind, text) pairs of all significant tokens.
fn significant_pairs(src: &str) -> Vec<(SyntaxKind, &str)> {
    tokenize(src)
        .into_iter()
        .filter(|t| !t.kind.is_trivia() && t.kind != SyntaxKind::Eof)
        .map(|t| (t.kind, &src[t.range.as_usize()]))
        .collect()
}

// --- Positive lexing -------------------------------------------------------

#[test]
fn keywords_vs_context_words() {
    // A lowercase keyword lexes as its keyword kind...
    assert_eq!(significant_kinds("state"), [SyntaxKind::StateKw]);
    // ...but the capitalized form is an ordinary identifier (case matters).
    assert_eq!(significant_kinds("State"), [SyntaxKind::Ident]);
    // Context words (viso, empty, prelude type names) are identifiers.
    assert_eq!(significant_kinds("viso"), [SyntaxKind::Ident]);
    assert_eq!(significant_kinds("empty"), [SyntaxKind::Ident]);
    assert_eq!(significant_kinds("Bool"), [SyntaxKind::Ident]);
    // A representative spread of keyword families.
    assert_eq!(significant_kinds("component"), [SyntaxKind::ComponentKw]);
    assert_eq!(significant_kinds("view"), [SyntaxKind::ViewKw]);
    assert_eq!(significant_kinds("shader"), [SyntaxKind::ShaderKw]);
    assert_eq!(significant_kinds("true"), [SyntaxKind::TrueKw]);
    assert_eq!(significant_kinds("self"), [SyntaxKind::SelfValueKw]);
    // A reserved-but-forbidden word gets its own kind (for a precise diagnostic).
    assert_eq!(significant_kinds("unsafe"), [SyntaxKind::UnsafeKw]);
}

#[test]
fn identifiers_and_raw_identifiers() {
    assert_eq!(significant_kinds("foo_bar123"), [SyntaxKind::Ident]);
    assert_eq!(significant_kinds("_leading"), [SyntaxKind::Ident]);
    // A raw identifier escapes a keyword.
    assert_eq!(
        significant_pairs("r#state"),
        [(SyntaxKind::RawIdent, "r#state")]
    );
    // A bare `r` and an ident starting with `r` are ordinary identifiers.
    assert_eq!(significant_kinds("r"), [SyntaxKind::Ident]);
    assert_eq!(significant_kinds("rust"), [SyntaxKind::Ident]);
    // Unicode identifiers (XID approximation via std char predicates).
    assert_eq!(significant_kinds("café"), [SyntaxKind::Ident]);
    assert_eq!(significant_kinds("日本語"), [SyntaxKind::Ident]);
}

#[test]
fn integer_and_float_literals() {
    assert_eq!(significant_kinds("42"), [SyntaxKind::IntLiteral]);
    assert_eq!(significant_kinds("0"), [SyntaxKind::IntLiteral]);
    assert_eq!(significant_kinds("0xFF"), [SyntaxKind::IntLiteral]);
    assert_eq!(significant_kinds("0o755"), [SyntaxKind::IntLiteral]);
    assert_eq!(significant_kinds("0b1010"), [SyntaxKind::IntLiteral]);
    assert_eq!(significant_kinds("1_000_000"), [SyntaxKind::IntLiteral]);
    assert_eq!(significant_kinds("3.14"), [SyntaxKind::FloatLiteral]);
    assert_eq!(significant_kinds("1.0e10"), [SyntaxKind::FloatLiteral]);
    assert_eq!(significant_kinds("2.5E-3"), [SyntaxKind::FloatLiteral]);
    assert_eq!(significant_kinds("1e6"), [SyntaxKind::FloatLiteral]);
}

#[test]
fn string_and_char_and_color_literals() {
    assert_eq!(significant_kinds(r#""hello""#), [SyntaxKind::StringLiteral]);
    assert_eq!(
        significant_kinds(r#""with \"escapes\" and \n\t\u{1F600}""#),
        [SyntaxKind::StringLiteral]
    );
    assert_eq!(
        significant_pairs(r##"r"raw \n stays""##),
        [(SyntaxKind::RawStringLiteral, r#"r"raw \n stays""#)]
    );
    assert_eq!(significant_kinds("'a'"), [SyntaxKind::CharLiteral]);
    assert_eq!(significant_kinds(r"'\n'"), [SyntaxKind::CharLiteral]);
    for c in ["#f00", "#ff0000", "#ff0000ff", "#abcd"] {
        assert_eq!(
            significant_kinds(c),
            [SyntaxKind::ColorLiteral],
            "color {c}"
        );
        assert!(tokenize(c)[0].error.is_none(), "well-formed color {c}");
    }
}

#[test]
fn operators_and_delimiters() {
    use SyntaxKind::*;
    let cases: &[(&str, SyntaxKind)] = &[
        ("(", LParen),
        (")", RParen),
        ("{", LBrace),
        ("}", RBrace),
        ("[", LBracket),
        ("]", RBracket),
        (",", Comma),
        (";", Semi),
        (":", Colon),
        ("::", ColonColon),
        ("@", At),
        ("=", Eq),
        ("+=", PlusEq),
        ("-=", MinusEq),
        ("*=", StarEq),
        ("/=", SlashEq),
        ("%=", PercentEq),
        ("&=", AmpEq),
        ("|=", PipeEq),
        ("^=", CaretEq),
        ("<<=", ShlEq),
        (">>=", ShrEq),
        ("+", Plus),
        ("-", Minus),
        ("*", Star),
        ("/", Slash),
        ("!", Bang),
        ("~", Tilde),
        ("&", Amp),
        ("|", Pipe),
        ("^", Caret),
        ("<<", Shl),
        (">>", Shr),
        ("==", EqEq),
        ("!=", Neq),
        ("<", Lt),
        ("<=", Le),
        (">", Gt),
        (">=", Ge),
        ("&&", AmpAmp),
        ("||", PipePipe),
        ("??", QuestionQuestion),
        (".", Dot),
        ("?.", QuestionDot),
        ("?", Question),
        ("..", DotDot),
        ("..=", DotDotEq),
        ("->", Arrow),
        ("=>", FatArrow),
        ("<=>", BidiArrow),
    ];
    for &(src, kind) in cases {
        assert_eq!(significant_kinds(src), [kind], "operator {src:?}");
    }
}

#[test]
fn comments_and_doc_comments() {
    assert_eq!(significant_kinds("// line\n"), []); // trivia only
    let toks = tokenize("/// doc\n//! mod\n/* block */\n/* /* nested */ */");
    let kinds: Vec<_> = toks
        .iter()
        .filter(|t| t.kind != SyntaxKind::Whitespace && t.kind != SyntaxKind::Eof)
        .map(|t| t.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            SyntaxKind::DocComment,
            SyntaxKind::ModuleDocComment,
            SyntaxKind::BlockComment,
            SyntaxKind::BlockComment,
        ]
    );
    // `////` is an ordinary line comment, not a doc comment.
    assert_eq!(tokenize("//// not doc\n")[0].kind, SyntaxKind::LineComment);
}

// --- Disambiguation --------------------------------------------------------

#[test]
fn percent_unit_vs_modulo() {
    // `50%` at a clean boundary is a unit literal.
    assert_eq!(significant_kinds("50%"), [SyntaxKind::UnitLiteral]);
    assert_eq!(significant_kinds("50% "), [SyntaxKind::UnitLiteral]);
    assert_eq!(
        significant_kinds("width: 50%;"),
        [
            SyntaxKind::Ident,
            SyntaxKind::Colon,
            SyntaxKind::UnitLiteral,
            SyntaxKind::Semi,
        ]
    );
    // `50%3` and `50 % 3` are modulo, not a percent suffix.
    assert_eq!(
        significant_kinds("50%3"),
        [
            SyntaxKind::IntLiteral,
            SyntaxKind::Percent,
            SyntaxKind::IntLiteral,
        ]
    );
    assert_eq!(
        significant_kinds("50 % 3"),
        [
            SyntaxKind::IntLiteral,
            SyntaxKind::Percent,
            SyntaxKind::IntLiteral,
        ]
    );
    // `50%x` is modulo against an identifier.
    assert_eq!(
        significant_kinds("50%x"),
        [
            SyntaxKind::IntLiteral,
            SyntaxKind::Percent,
            SyntaxKind::Ident,
        ]
    );
}

#[test]
fn range_after_integer() {
    // `1..2` is Int, `..`, Int — not a malformed float (spec forces `0.5`/`1.0`).
    assert_eq!(
        significant_kinds("1..2"),
        [
            SyntaxKind::IntLiteral,
            SyntaxKind::DotDot,
            SyntaxKind::IntLiteral,
        ]
    );
    assert_eq!(
        significant_kinds("0..=10"),
        [
            SyntaxKind::IntLiteral,
            SyntaxKind::DotDotEq,
            SyntaxKind::IntLiteral,
        ]
    );
    // `1.` is Int followed by Dot (no trailing-dot float).
    assert_eq!(
        significant_kinds("1.foo"),
        [SyntaxKind::IntLiteral, SyntaxKind::Dot, SyntaxKind::Ident,]
    );
}

#[test]
fn unit_suffix_literals() {
    // A suffix immediately after digits makes a unit literal.
    for src in ["12px", "1u32", "3.0f32", "16dp"] {
        assert_eq!(
            significant_kinds(src),
            [SyntaxKind::UnitLiteral],
            "unit {src}"
        );
    }
    // A space breaks the suffix: two tokens.
    assert_eq!(
        significant_kinds("12 px"),
        [SyntaxKind::IntLiteral, SyntaxKind::Ident]
    );
}

#[test]
fn raw_string_hash_matching() {
    // A single-hash raw string closes at the first `"#`. So an interior `"#`
    // ends it early: `r#"a "#` is the literal, and ` inside"#` is what follows.
    let src = r####"r#"a "# inside"#"####;
    let toks = significant_pairs(src);
    assert_eq!(toks[0], (SyntaxKind::RawStringLiteral, r##"r#"a "#"##));
    // A two-hash raw string is needed to embed a `"#` sequence intact.
    let src2 = r####"r##"a "# inside"##"####;
    let toks2 = significant_pairs(src2);
    assert_eq!(toks2.len(), 1);
    assert_eq!(toks2[0], (SyntaxKind::RawStringLiteral, src2));
    // `r#ident` is a raw identifier, not a raw string.
    assert_eq!(
        significant_pairs("r#foo"),
        [(SyntaxKind::RawIdent, "r#foo")]
    );
}

#[test]
fn numeric_separator_rules() {
    // Well-placed separators are fine.
    assert!(tokenize("1_000")[0].error.is_none());
    // Leading, trailing, and doubled separators are flagged.
    for bad in ["1__0", "1_", "0x_FF"] {
        let tok = tokenize(bad)[0];
        assert_eq!(
            tok.error,
            Some(LexError::MalformedNumericSeparator),
            "expected separator error for {bad:?}"
        );
    }
}

#[test]
fn escape_bounds() {
    // A valid `\xNN` and `\u{...}`.
    assert!(tokenize(r#""\x41 \u{41}""#)[0].error.is_none());
    // `\x` with too few hex digits.
    assert_eq!(
        tokenize(r#""\x4""#)[0].error,
        Some(LexError::InvalidByteEscape)
    );
    // `\u{...}` with no digits / not a scalar.
    assert_eq!(
        tokenize(r#""\u{}""#)[0].error,
        Some(LexError::InvalidUnicodeEscape)
    );
    assert_eq!(
        tokenize(r#""\u{110000}""#)[0].error,
        Some(LexError::InvalidUnicodeEscape)
    );
    // An unknown escape letter.
    assert_eq!(tokenize(r#""\q""#)[0].error, Some(LexError::InvalidEscape));
}

// --- Malformed-input recovery ----------------------------------------------

#[test]
fn unterminated_recovers_and_keeps_lexing() {
    // An unterminated string is flagged; lexing continues to EOF (no panic).
    let toks = tokenize("\"unterminated");
    assert_eq!(toks[0].kind, SyntaxKind::StringLiteral);
    assert_eq!(toks[0].error, Some(LexError::UnterminatedString));
    assert_eq!(toks.last().unwrap().kind, SyntaxKind::Eof);

    // A string closed by a newline is unterminated but the next line still lexes.
    let toks = tokenize("\"oops\nlet x");
    assert_eq!(toks[0].error, Some(LexError::UnterminatedString));
    let kinds: Vec<_> = toks
        .iter()
        .filter(|t| !t.kind.is_trivia() && t.kind != SyntaxKind::Eof)
        .map(|t| t.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            SyntaxKind::StringLiteral,
            SyntaxKind::LetKw,
            SyntaxKind::Ident
        ]
    );

    // An unterminated nested block comment.
    let toks = tokenize("/* /* still open");
    assert_eq!(toks[0].kind, SyntaxKind::BlockComment);
    assert_eq!(toks[0].error, Some(LexError::UnterminatedBlockComment));

    // A character literal with too many characters.
    assert_eq!(tokenize("'ab'")[0].error, Some(LexError::MalformedChar));
    // An unterminated char.
    assert_eq!(tokenize("'a")[0].error, Some(LexError::UnterminatedChar));
}

#[test]
fn bad_color_and_stray_chars() {
    // A `#` followed by a non-color run is flagged but still one span.
    assert_eq!(tokenize("#xyz")[0].error, Some(LexError::InvalidColor));
    assert_eq!(tokenize("#12345")[0].error, Some(LexError::InvalidColor));
    // A NUL byte is flagged.
    assert_eq!(tokenize("\0")[0].error, Some(LexError::NulInSource));
    // A bare carriage return is flagged as whitespace with an error.
    let toks = tokenize("\rx");
    assert_eq!(toks[0].kind, SyntaxKind::Whitespace);
    assert_eq!(toks[0].error, Some(LexError::BareCarriageReturn));
}

// --- Losslessness ----------------------------------------------------------

#[test]
fn cst_round_trips_source() {
    let corpus = [
        "component Counter { state count: 0; view { Text { text: count; } } }",
        "// leading comment\n\nlet x = 50%;\n",
        "\"unterminated string across\nlines",
        "/* nested /* block */ comment */ 0xFF 3.14 #ff00ff",
        "50%3 1..2 r#raw r\"raw string\" 'c'",
        "   \t\n  weird\r\n whitespace  ",
        "",
        "\0\0malformed\0",
    ];
    for src in corpus {
        let tokens = tokenize(src);
        let tree = flat_tree(&tokens, src);
        assert_eq!(tree.text(), src, "flat tree must round-trip {src:?}");

        let parsed = parse(&tokens, src);
        assert_eq!(
            parsed.root.text(),
            src,
            "parsed tree must round-trip {src:?}"
        );
    }
}

// --- Incremental re-lex ----------------------------------------------------

#[test]
fn incremental_equals_full_relex() {
    let base = "component App { state n: 0; view { Text { text: n; } } }";
    // A spread of single edits: insert, delete, replace, at various positions.
    let edits: &[(usize, usize, &str)] = &[
        (16, 16, "count"),                            // insert into an identifier
        (0, 0, "// note\n"),                          // prepend a comment
        (base.len(), base.len(), "\nlet extra = 1;"), // append
        (23, 24, ": 42"),                             // replace a run
        (10, 13, ""),                                 // delete inside a keyword-ish span
    ];
    for &(start, end, insert) in edits {
        let edit = Edit::new(
            TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32)),
            insert,
        );
        let new_source = edit.apply(base);
        let old_tokens = tokenize(base);
        let incremental = reparse_tokens(&old_tokens, &edit, &new_source);
        let full = tokenize(&new_source);
        assert_eq!(
            incremental, full,
            "incremental re-lex must equal full re-lex for edit {start}..{end} {insert:?}"
        );
    }
}

// --- Coordinate map --------------------------------------------------------

#[test]
fn line_index_coordinates() {
    // A source mixing ASCII and multi-byte chars on line 1.
    let src = "let a = 1;\nlet 日 = 2;\n";
    let index = LineIndex::new(src);
    assert_eq!(index.line_count(), 3); // two newlines → three lines

    // The `日` starts at a known byte offset; its scalar/utf16 columns differ
    // from its byte column because it is a 3-byte / 1-utf16-unit char.
    let ni = src.find('日').unwrap() as u32;
    let off = TextSize::new(ni);
    let byte_col = index.line_col_utf8(off).column;
    let scalar_col = index.line_col_scalar(off).column;
    let utf16_col = index.line_col_utf16(off).column;
    // `日` is the 5th char on line 1 (`l e t _ 日` → col 4 in scalars).
    assert_eq!(scalar_col, 4);
    assert_eq!(utf16_col, 4);
    // In bytes it is still col 4 (everything before it is ASCII).
    assert_eq!(byte_col, 4);

    // The char *after* `日`: byte col jumps by 3, scalar/utf16 by 1.
    let after = TextSize::new(ni + '日'.len_utf8() as u32);
    assert_eq!(index.line_col_utf8(after).column, 7);
    assert_eq!(index.line_col_scalar(after).column, 5);
    assert_eq!(index.line_col_utf16(after).column, 5);
}

// --- Fuzz smoke ------------------------------------------------------------

#[test]
fn never_panics_on_arbitrary_input() {
    // A cheap deterministic pseudo-random byte generator (xorshift) — the lexer
    // must never panic and must always terminate with an Eof token.
    let mut state: u64 = 0x9E3779B97F4A7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..2000 {
        let len = (next() % 64) as usize;
        // Build a string from random Unicode scalars so the input is valid UTF-8
        // (the lexer's contract is over `&str`); arbitrary bytes are the caller's
        // problem before this boundary.
        let s: String = (0..len)
            .map(|_| {
                let cp = (next() % 0x2FF) as u32;
                char::from_u32(cp).unwrap_or('?')
            })
            .collect();
        let toks = tokenize(&s);
        assert_eq!(toks.last().unwrap().kind, SyntaxKind::Eof);
        // Losslessness must hold even for garbage.
        let tree = flat_tree(&toks, &s);
        assert_eq!(tree.text(), s);
    }
}
