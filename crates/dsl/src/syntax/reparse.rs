//! Single-edit incremental re-lex.
//!
//! Editors re-lex on every keystroke; re-tokenizing a whole file each time is
//! wasteful when one character changed. This module re-lexes only the region an
//! edit touched, reusing the token spans on either side. The guiding invariant —
//! the acceptance contract for the slice — is that the incremental token stream
//! is **identical** to a full re-lex of the edited source. When any assumption
//! that would break that identity does not hold, this falls back to a full
//! re-lex, so correctness never depends on the fast path being taken.
//!
//! The `.vs` lexer carries no state across token boundaries (see
//! [`LexState`](super::lexer::LexState)), so a re-lex can safely restart at any
//! token boundary at or before the edit. The one subtlety is that a token
//! adjacent to the edit may merge with or split from its neighbor (typing `x`
//! after `foo` extends the identifier), so the reused prefix ends a few tokens
//! *before* the edit and the reused suffix begins a few tokens *after* it; the
//! seam is re-lexed and the result stitched. This slice re-lexes from the token
//! boundary at or before the edit start to end of input and reuses only the
//! untouched prefix — a correct, conservative version of that idea; a tighter
//! suffix-reuse pass can follow when profiling shows it matters.

use super::lexer::Lexer;
use super::span::{TextRange, TextSize};
use super::token::Token;

/// A single contiguous text edit: the byte range `range` in the old source is
/// replaced by `insert`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edit<'a> {
    /// The byte range in the *old* source that was replaced.
    pub range: TextRange,
    /// The text spliced in where `range` used to be.
    pub insert: &'a str,
}

impl<'a> Edit<'a> {
    /// An edit replacing `range` of the old source with `insert`.
    pub fn new(range: TextRange, insert: &'a str) -> Edit<'a> {
        Edit { range, insert }
    }

    /// Applies the edit to `old`, returning the new source. Panics only if
    /// `range` is out of bounds or not on char boundaries (a caller bug).
    pub fn apply(&self, old: &str) -> String {
        let r = self.range.as_usize();
        let mut out = String::with_capacity(old.len() - (r.end - r.start) + self.insert.len());
        out.push_str(&old[..r.start]);
        out.push_str(self.insert);
        out.push_str(&old[r.end..]);
        out
    }
}

/// Incrementally re-lexes after a single `edit`, given the previous token stream
/// `old_tokens` (over the old source) and the already-edited `new_source`.
///
/// Returns the full new token stream (trivia and final `Eof` included), and
/// carries the guarantee that it equals [`tokenize`](super::lexer::tokenize) of
/// `new_source`. Reuses the untouched token prefix before the edit and re-lexes
/// the remainder.
pub fn reparse_tokens(old_tokens: &[Token], edit: &Edit, new_source: &str) -> Vec<Token> {
    let edit_start = edit.range.start();

    // Find the last token that ends at or before the edit start AND sits on a
    // char boundary of the new source at the same offset (offsets before the
    // edit are unchanged by construction). Reuse everything strictly before it as
    // the prefix, and re-lex from its start so a token abutting the edit is
    // re-evaluated (it may merge with the inserted text).
    let prefix_len = reusable_prefix(old_tokens, edit_start);
    let resume_at = if prefix_len == 0 {
        TextSize::ZERO
    } else {
        old_tokens[prefix_len - 1].range.end()
    };

    // Guard: resume offset must be a char boundary in the new source and within
    // bounds. If not (an edit landed mid-multibyte or past end), fall back to a
    // full re-lex — always correct, never a panic.
    let resume = resume_at.to_usize();
    if resume > new_source.len() || !new_source.is_char_boundary(resume) {
        return super::lexer::tokenize(new_source);
    }

    let mut out: Vec<Token> = old_tokens[..prefix_len].to_vec();
    let mut lexer = Lexer::resume(new_source, resume_at, Default::default());
    loop {
        let tok = lexer.next_token();
        let is_eof = tok.kind == super::kind::SyntaxKind::Eof;
        out.push(tok);
        if is_eof {
            break;
        }
    }
    out
}

/// The number of leading tokens whose spans are entirely before `edit_start` and
/// are therefore reusable verbatim. The token abutting or containing the edit is
/// excluded, so it gets re-lexed (it may merge with inserted text).
fn reusable_prefix(old_tokens: &[Token], edit_start: TextSize) -> usize {
    let mut n = 0;
    for tok in old_tokens {
        // Stop at the first token whose end reaches the edit start — that token
        // (and everything after) must be re-lexed. The `Eof` token has a
        // zero-width range at end of input, so it is naturally excluded here.
        if tok.range.end().to_u32() >= edit_start.to_u32() {
            break;
        }
        n += 1;
    }
    n
}
