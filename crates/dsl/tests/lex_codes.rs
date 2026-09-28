//! Pins every lexer-error code: each [`LexError`] maps to its stable Appendix C
//! code, the spec's Appendix C table has a row for it, and a real source snippet
//! triggers it.

use viso_dsl::syntax::{LexError, tokenize};

/// The Viso DSL specification, whose Appendix C is the code registry.
const SPEC: &str = include_str!("../../../Viso_DSL_1.0.md");

/// Every variant, listed once.
const ALL: &[LexError] = &[
    LexError::UnterminatedBlockComment,
    LexError::UnterminatedString,
    LexError::UnterminatedRawString,
    LexError::UnterminatedChar,
    LexError::MalformedChar,
    LexError::InvalidEscape,
    LexError::InvalidByteEscape,
    LexError::InvalidUnicodeEscape,
    LexError::InvalidColor,
    LexError::MalformedNumericSeparator,
    LexError::MalformedIntLiteral,
    LexError::MisplacedSuffix,
    LexError::UnknownSuffix,
    LexError::TooManyRawStringHashes,
    LexError::BareCarriageReturn,
    LexError::NulInSource,
    LexError::ConfusableIdent,
    LexError::UnexpectedCharacter,
];

/// The frozen code of `error` and a snippet that produces it.
fn pinned(error: LexError) -> (&'static str, String) {
    let (code, snippet) = match error {
        LexError::ConfusableIdent => ("E1102", "p\u{430}y"),
        LexError::UnterminatedString => ("E1201", "\"open"),
        LexError::UnterminatedRawString => ("E1201", "r#\"open"),
        LexError::UnterminatedChar => ("E1201", "'a"),
        LexError::UnterminatedBlockComment => ("E1202", "/* open"),
        LexError::MalformedIntLiteral => ("E1203", "0b12"),
        LexError::MisplacedSuffix => ("E1203", "1e2dp"),
        LexError::UnknownSuffix => ("E1204", "12pt"),
        LexError::InvalidEscape => ("E1205", r#""\q""#),
        LexError::InvalidByteEscape => ("E1205", r#""\x4""#),
        LexError::InvalidUnicodeEscape => ("E1205", r#""\u{110000}""#),
        LexError::MalformedNumericSeparator => ("E1206", "1__0"),
        LexError::InvalidColor => ("E1207", "#12345"),
        LexError::MalformedChar => ("E1208", "'ab'"),
        LexError::BareCarriageReturn => ("E1209", "\rx"),
        LexError::NulInSource => ("E1209", "\0"),
        LexError::UnexpectedCharacter => ("E1209", "¡"),
        LexError::TooManyRawStringHashes => {
            return ("E1210", format!("r{}\"x", "#".repeat(256)));
        }
        other => panic!("{other:?} has no pinned code"),
    };
    (code, snippet.to_string())
}

#[test]
fn every_variant_keeps_its_frozen_code() {
    for &error in ALL {
        assert_eq!(
            error.code(),
            pinned(error).0,
            "{error:?} changed its stable code"
        );
    }
}

#[test]
fn every_code_has_an_appendix_c_row() {
    let appendix = &SPEC[SPEC.find("# 附录 C").expect("Appendix C heading")..];
    for &error in ALL {
        let code = pinned(error).0;
        assert!(
            appendix.contains(&format!("| {code} ")),
            "{error:?} ({code}) has no Appendix C row"
        );
    }
}

#[test]
fn every_variant_is_reachable_from_source() {
    for &error in ALL {
        let (_, src) = pinned(error);
        let found: Vec<_> = tokenize(&src).iter().filter_map(|t| t.error).collect();
        assert!(
            found.contains(&error),
            "{src:?} did not report {error:?}, got {found:?}"
        );
    }
}
