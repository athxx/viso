//! Syntax highlighting: a language splits each line into tokens, given the
//! state the line before left it in, and the editor keeps each line's end
//! state so an edit re-highlights only from the line it changed.
//!
//! A [`Language`] is a lexer a line at a time: [`Language::line`] takes the
//! state the previous line ended in — inside a block comment, a string that
//! runs on, nothing — and hands back the line's [`Token`]s and the state it
//! ends in. States are `u32`s the language picks the meaning of; zero is the
//! start of a document. That is all the editor asks: no grammar, no regular
//! expressions, no scopes — a language is as fast as its lexer.
//!
//! The editor keeps the end state of every line up to the last one it has
//! needed. An edit drops them from its first changed line on; the next line
//! drawn re-lexes from there, and stops early at the first line past the
//! edit that ends in the state it ended in before: every line after it is
//! as it was. Lines past the screen are lexed only when they are needed.

use std::ops::Range;

/// What a token is: the look a theme gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum TokenKind {
    /// Plain text, whitespace.
    #[default]
    Text,
    /// A keyword: `fn`, `if`, `return`.
    Keyword,
    /// A keyword that names a value: `true`, `self`, `null`.
    Builtin,
    /// A type name.
    Type,
    /// A function's name, called or declared.
    Function,
    /// A macro's name.
    Macro,
    /// A variable or other plain name.
    Variable,
    /// A parameter's name.
    Parameter,
    /// A field or property name.
    Property,
    /// A constant, an enum member.
    Constant,
    /// A namespace or module path segment.
    Namespace,
    /// A number.
    Number,
    /// A string.
    String,
    /// An escape in a string.
    Escape,
    /// A character literal.
    Char,
    /// A comment.
    Comment,
    /// A documentation comment.
    DocComment,
    /// An operator.
    Operator,
    /// Punctuation: `,` `;` `.` `:`.
    Punctuation,
    /// A bracket: `(` `[` `{` and their pairs.
    Bracket,
    /// An attribute or annotation.
    Attribute,
    /// A label or lifetime.
    Label,
    /// Text the lexer rejects.
    Invalid,
}

impl TokenKind {
    /// Every kind, in order: what a theme is a table of.
    pub const ALL: [TokenKind; 23] = [
        TokenKind::Text,
        TokenKind::Keyword,
        TokenKind::Builtin,
        TokenKind::Type,
        TokenKind::Function,
        TokenKind::Macro,
        TokenKind::Variable,
        TokenKind::Parameter,
        TokenKind::Property,
        TokenKind::Constant,
        TokenKind::Namespace,
        TokenKind::Number,
        TokenKind::String,
        TokenKind::Escape,
        TokenKind::Char,
        TokenKind::Comment,
        TokenKind::DocComment,
        TokenKind::Operator,
        TokenKind::Punctuation,
        TokenKind::Bracket,
        TokenKind::Attribute,
        TokenKind::Label,
        TokenKind::Invalid,
    ];

    /// Whether text of this kind is a comment or a string: where brackets
    /// do not pair and a typed quote does not close.
    pub fn is_literal(self) -> bool {
        matches!(
            self,
            TokenKind::Comment
                | TokenKind::DocComment
                | TokenKind::String
                | TokenKind::Escape
                | TokenKind::Char
        )
    }
}

/// `len` bytes of a line of one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub len: u32,
    pub kind: TokenKind,
}

impl Token {
    pub fn new(len: usize, kind: TokenKind) -> Token {
        Token {
            len: len as u32,
            kind,
        }
    }
}

/// A language the editor edits: how it lexes a line, and the few facts of
/// its syntax editing commands use.
pub trait Language: 'static {
    /// Its name, for a status line.
    fn name(&self) -> &str;

    /// Splits `line` (without its break) into `out`, which comes empty,
    /// starting in `state`, the state the line before ended in (zero for
    /// the first), and returns the state it ends in. The tokens cover the
    /// line in order; bytes they leave out are plain text.
    fn line(&self, state: u32, line: &str, out: &mut Vec<Token>) -> u32;

    /// What a line comment starts with: what toggling a comment puts in.
    fn line_comment(&self) -> Option<&str> {
        None
    }

    /// The pairs typing an opener closes, and typing the closer steps over.
    fn auto_pairs(&self) -> &[(char, char)] {
        &[('(', ')'), ('[', ']'), ('{', '}'), ('"', '"')]
    }

    /// Whether a line indents the one after it: it ends in an opener.
    fn indents_after(&self, line: &str) -> bool {
        let line = line.trim_end();
        line.ends_with('{') || line.ends_with('(') || line.ends_with('[')
    }
}

/// Plain text: one token a line.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlainText;

impl Language for PlainText {
    fn name(&self) -> &str {
        "Plain Text"
    }

    fn line(&self, _: u32, _: &str, _: &mut Vec<Token>) -> u32 {
        0
    }

    fn auto_pairs(&self) -> &[(char, char)] {
        &[]
    }

    fn indents_after(&self, _: &str) -> bool {
        false
    }
}

/// The end state of each line, as far as the editor has lexed.
#[derive(Debug, Clone, Default)]
pub struct LineStates {
    /// One per line; right for `..valid`.
    states: Vec<u32>,
    /// The lines whose end states are right.
    valid: usize,
    /// Lines past `valid` whose states were right before an edit: right
    /// again from the first line past the edit that ends as it did.
    known: usize,
    /// The first line no edit since the last lex changed.
    dirty_end: usize,
}

impl LineStates {
    /// States for `lines` lines, none lexed.
    pub fn new(lines: usize) -> LineStates {
        LineStates {
            states: vec![0; lines],
            valid: 0,
            known: 0,
            dirty_end: 0,
        }
    }

    /// The lines lexed.
    pub fn valid(&self) -> usize {
        self.valid
    }

    /// Follows an edit: lines `splice` replaced.
    pub fn splice(&mut self, splices: &[crate::LineSplice]) {
        for s in splices.iter().rev() {
            let (a, r, n) = (s.old, s.removed, s.added);
            let end = (a + r).min(self.states.len());
            // The replaced lines' last end state goes on the last line put
            // in their place: when it ends as they did, so do the lines
            // after.
            let last = end.checked_sub(1).map_or(0, |l| self.states[l]);
            self.states.splice(
                a.min(end)..end,
                std::iter::repeat_n(0, n - 1).chain(std::iter::once(last)),
            );
            let shift = |at: usize| {
                if at >= a + r { at + n - r } else { at.min(a) }
            };
            let dirty = self.dirty_end > self.valid;
            self.valid = self.valid.min(a);
            self.known = shift(self.known);
            self.dirty_end = if dirty && self.dirty_end > a + r {
                self.dirty_end + n - r
            } else {
                a + n
            };
        }
    }

    /// Forgets every state: the language changed.
    pub fn reset(&mut self, lines: usize) {
        *self = LineStates::new(lines);
    }

    /// The state line `line` starts in, lexing the lines before it that
    /// are not yet with `lex`.
    pub fn start_of(&mut self, line: usize, mut lex: impl FnMut(usize, u32) -> u32) -> u32 {
        while self.valid < line {
            let i = self.valid;
            let start = if i == 0 { 0 } else { self.states[i - 1] };
            let end = lex(i, start);
            if i + 1 >= self.dirty_end && i < self.known && end == self.states[i] {
                // Ends as it did before the edit: so do the lines after.
                self.valid = self.known;
                continue;
            }
            self.states[i] = end;
            self.valid = i + 1;
        }
        self.known = self.known.max(self.valid);
        if line == 0 { 0 } else { self.states[line - 1] }
    }
}

/// The tokens of a line as byte ranges, plain text filled in between.
pub fn spans(tokens: &[Token], len: usize) -> impl Iterator<Item = (Range<usize>, TokenKind)> + '_ {
    let mut at = 0usize;
    tokens.iter().filter_map(move |t| {
        let start = at.min(len);
        at += t.len as usize;
        let end = at.min(len);
        (end > start).then_some((start..end, t.kind))
    })
}

/// The kind of token at byte `at` of a line.
pub fn kind_at(tokens: &[Token], at: usize) -> TokenKind {
    let mut start = 0usize;
    for t in tokens {
        let end = start + t.len as usize;
        if at < end {
            return t.kind;
        }
        start = end;
    }
    TokenKind::Text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Change, ChangeSet, Document};

    /// `/* */` comments over lines: state 1 inside one.
    struct Blocks;

    impl Language for Blocks {
        fn name(&self) -> &str {
            "blocks"
        }

        fn line(&self, mut state: u32, line: &str, out: &mut Vec<Token>) -> u32 {
            let mut start = 0;
            let mut i = 0;
            let b = line.as_bytes();
            while i < b.len() {
                if state == 0 && b[i..].starts_with(b"/*") {
                    if i > start {
                        out.push(Token::new(i - start, TokenKind::Text));
                    }
                    start = i;
                    state = 1;
                    i += 2;
                } else if state == 1 && b[i..].starts_with(b"*/") {
                    i += 2;
                    out.push(Token::new(i - start, TokenKind::Comment));
                    start = i;
                    state = 0;
                } else {
                    i += 1;
                }
            }
            if i > start {
                let kind = if state == 1 {
                    TokenKind::Comment
                } else {
                    TokenKind::Text
                };
                out.push(Token::new(i - start, kind));
            }
            state
        }
    }

    #[test]
    fn an_edit_relexes_only_until_the_states_agree_again() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let mut doc = Document::new(&text);
        let mut states = LineStates::new(doc.line_count());
        let mut lexed = 0;
        let lex = |doc: &Document, i: usize, s: u32, lexed: &mut usize| {
            *lexed += 1;
            Blocks.line(s, &doc.line(i), &mut Vec::new())
        };
        states.start_of(50, |i, s| lex(&doc, i, s, &mut lexed));
        assert_eq!(lexed, 50);
        // An edit on line 10 that changes no state: line 10 relexed, the
        // rest known.
        lexed = 0;
        let at = doc.line_start(10);
        let applied = doc.apply(&ChangeSet::one(Change::insert(at, "x")));
        states.splice(&applied.splices);
        states.start_of(50, |i, s| lex(&doc, i, s, &mut lexed));
        assert_eq!(lexed, 1);
        // Opening a comment on line 20 relexes on to the end of what is
        // asked.
        lexed = 0;
        let at = doc.line_start(20);
        let applied = doc.apply(&ChangeSet::one(Change::insert(at, "/*")));
        states.splice(&applied.splices);
        assert_eq!(states.start_of(40, |i, s| lex(&doc, i, s, &mut lexed)), 1);
        assert_eq!(lexed, 20);
        // Closing it again on line 30 converges past it.
        lexed = 0;
        let at = doc.line_start(30);
        let applied = doc.apply(&ChangeSet::one(Change::insert(at, "*/")));
        states.splice(&applied.splices);
        assert_eq!(states.start_of(45, |i, s| lex(&doc, i, s, &mut lexed)), 0);
        // Line 40 ends as it did before the comment opened: known again.
        assert_eq!(lexed, 11);
        // Lines added and removed keep the states after them.
        lexed = 0;
        let at = doc.line_start(5);
        let applied = doc.apply(&ChangeSet::one(Change::insert(at, "a\nb\nc\n")));
        states.splice(&applied.splices);
        assert_eq!(states.start_of(50, |i, s| lex(&doc, i, s, &mut lexed)), 0);
        assert_eq!(lexed, 4);
        // Every state agrees with lexing from scratch.
        let mut fresh = LineStates::new(doc.line_count());
        for l in 0..doc.line_count() {
            let mut n = 0;
            assert_eq!(
                states.start_of(l, |i, s| lex(&doc, i, s, &mut n)),
                fresh.start_of(l, |i, s| lex(&doc, i, s, &mut n)),
                "line {l}"
            );
        }
    }

    #[test]
    fn spans_cover_the_line() {
        let tokens = [
            Token::new(3, TokenKind::Keyword),
            Token::new(1, TokenKind::Text),
            Token::new(4, TokenKind::Function),
        ];
        let spans: Vec<_> = spans(&tokens, 6).collect();
        assert_eq!(
            spans,
            [
                (0..3, TokenKind::Keyword),
                (3..4, TokenKind::Text),
                (4..6, TokenKind::Function)
            ]
        );
        assert_eq!(kind_at(&tokens, 5), TokenKind::Function);
        assert_eq!(kind_at(&tokens, 9), TokenKind::Text);
    }
}
