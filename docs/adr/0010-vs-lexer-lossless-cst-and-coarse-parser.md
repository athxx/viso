# ADR 0010 — `.vs` lexer, lossless green-tree CST, and the coarse parser skeleton

- Status: Accepted
- Date: 2026-09-03

## Context

Phase 6 opens the Viso DSL frontend. Slice K is its lexical layer: the streaming
tokenizer, the lossless concrete syntax tree (CST) every later stage consumes, and
a *minimal* parser skeleton. Governing spec is `Viso_DSL_1.0.md` §9–20 (lexical),
§113 (Token + Lossless CST), §152 (lexer/parser acceptance + fuzz); AGENTS §21.2
(pipeline), §30 (diagnostics), §52 (CST checklist), §68 (ADR trigger: "Viso DSL
language/module semantics").

The reference framework (`makepad/code_editor/src/{token,tokenizer}.rs`) is a
per-line syntax-highlight lexer: coarse kind + length, with a small per-line state
cache carried across lines. That is the right *idiom* for streaming and
state-carry, but far too coarse for a compiler frontend that must feed AST/HIR/IR,
a formatter, an LSP, and hot reload. Per the standing "take semantics, not coarse
architecture" rule, Viso diverges: precise token kinds, tri-coordinate spans,
trivia preserved as tokens, lexical-error recovery, and a rowan-style green tree.

This ADR records the lexical/CST decisions and the K/L scope split. The typed
grammar, AST, name resolution, and module graph are Slice L.

## Decision

### 1. Rowan-style green tree as the lossless CST

The CST is an immutable, offset-relative, `Rc`-shareable green tree (the
rust-analyzer design): a `GreenNode` stores its kind, a summed `text_len`, and a
`Vec<GreenChild>`; a `GreenToken` stores kind, text, and an optional `LexError`.
Nodes store *relative* text length, not absolute spans, so a subtree is position-
independent and shareable across incremental edits. This is cold-path compiler/
editor structure, so `Rc` sharing is explicitly allowed (AGENTS §7.2, §44).

Requirements met (§113): all tokens/comments/whitespace are preserved (trivia are
green tokens), the tree supports `ErrorNode` and a zero-width `MissingToken`, and
`root.text() == source` round-trips byte-for-byte for any input — valid or
malformed.

### 2. Byte-primary spans; the other two coordinates are computed on demand

Tokens carry a single byte-offset `TextRange`/`TextSize` (compact, `Copy`). §9/§113
require unicode-scalar and UTF-16 coordinates for LSP, but storing three ranges on
every token would triple the hot token vector for a cold-path need. Instead a
`LineIndex` is built once per source and converts a byte offset to scalar/UTF-16
line+column on demand. This satisfies "queryable in three coordinates" without
per-token triple storage.

### 3. Trivia are tokens in the flat stream; attachment happens in the tree

The lexer emits whitespace and comments as their own tokens in stream order, so the
flat `Vec<Token>` is already lossless. Leading/trailing trivia *attachment* to
significant nodes is a green-tree concern, not a flat-token field. The `Token`
record stays minimal: `{ kind, range, error }` — `raw_text` is `&source[range]`, a
slice, so tokens own no `String`.

### 4. Lexical disambiguation rules

- **`%` percent vs modulo (§19.2):** `50%` is a `UnitLiteral` only when `%`
  immediately follows digits *and* the next char is not a digit or ident-start.
  `50%3`, `50%x`, and `50 % 3` lex the `%` as a modulo operator. This is the §152
  acceptance case.
- **Range after integer (§15/16):** `.5` and `1.` are illegal floats (spec forces
  `0.5` / `1.0`), so `1..2` lexes as `Int` + `..` + `Int`, and `1.foo` as `Int` +
  `.` + `Ident`. A `.` only starts a fraction when a digit follows it.
- **Raw strings vs raw identifiers (§11/17):** `r#name` (one hash then ident-start)
  is a `RawIdent`; `r"…"` / `r#"…"#` is a raw string, matched by hash count. A
  single-hash raw string closes at the first `"#`, so embedding a literal `"#`
  requires two hashes (`r##"…"##`).
- **Numeric separators (§15):** leading, trailing, and doubled `_` are flagged
  (`MalformedNumericSeparator`); a well-placed `1_000` is clean.
- **Escapes (§17):** `\xNN` requires two hex digits; `\u{…}` requires a valid
  scalar; unknown escape letters are flagged — each with a specific `LexError`.

### 5. The lexer never panics and never stalls

The fuzz contract (§152) is two-part: no panic *and* forward progress on every
path. Every branch consumes at least one whole `char`, including the guard in
`ident_or_keyword` for a non-ASCII byte that the byte-level dispatch admits but the
`char`-level XID predicate rejects — it consumes one char as `Error` rather than
producing a zero-width token that would loop forever. Malformed input (unterminated
string / block comment / char, bad color, stray chars) produces a flagged token and
lexing continues. Unicode identifiers use std `char` predicates as a provisional XID
approximation; full XID/NFC/confusable handling is deferred to symbol-table entry in
Slice L.

### 6. Single-edit incremental re-lex

`reparse.rs` re-lexes only the region an edit touches, splicing unchanged token
prefix/suffix around it — the reference's line-state idiom generalized to a
byte-range boundary. §152 acceptance: the incremental token stream equals a full
re-lex of the edited source, asserted across insert/delete/replace edits.

### 7. Coarse parser skeleton, not the typed grammar

The parser is a coarse recursive-descent pass that produces the CST *shape* the
§113 contract requires, and nothing more:

- balanced `{…}` / `(…)` / `[…]` become `Block` nodes (nesting recurses);
- a top-level declaration keyword through its terminating `;` or `{…}` body becomes
  an `Item` node;
- on a structural error the parser emits an `ErrorNode` and **synchronizes** to the
  next `;` / `,` / `}` / declaration keyword, so one mistake does not cascade;
- multiple diagnostics are recorded per pass (`UnclosedDelimiter`,
  `UnmatchedCloser`, `UnexpectedTokens`, codes `Parse0001`–`Parse0003`), namespaced
  apart from the lexer's `Lex` codes (§30);
- every token lands under a node, so the parsed tree round-trips like the flat tree.

This is deliberately *not* the typed grammar: no EBNF productions, no `:` vs `=`
context split, no precedence-correct expressions, no generics. Those, plus AST,
name resolution, and the module graph, are Slice L. K produces a lossless,
error-recoverable CST that L refines into a typed grammar + AST.

### 8. `viso-ende` stays deferred; crate count and deps unchanged

The DSL byte-encoding subsystem (doc §9/§10.5) is not needed by any Slice K
consumer, so it stays deferred. The workspace remains **13 crates** and `viso-dsl`
gains **zero** new dependencies; `cargo xtask check-deps` stays green.

## Consequences

- Downstream stages (AST/HIR/IR, formatter, LSP, hot reload) build on one lossless,
  incrementally-reparsable CST rather than re-tokenizing.
- The green tree's `Rc` sharing is a cold-path allocation, acceptable per §7.2/§44
  but never to be copied into runtime hot paths.
- The coarse parser's node set (`Root`, `Item`, `Block`, `ErrorNode`) is
  intentionally small; Slice L extends `SyntaxKind` with the grammar's node kinds
  and refines `Block`/`Item` interiors without changing the losslessness contract.
- Verification for this slice: `cargo build/test/clippy/fmt -p viso-dsl` clean
  (18 lexer + 8 parser integration tests, including malformed-recovery, incremental
  re-lex, and never-hang/never-panic fuzz smokes), `cargo xtask check-deps` green.
  No shaders/UI/GPU touched, so no Metal/headless run was required.
