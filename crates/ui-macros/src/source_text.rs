//! An inline macro body as DSL source text, with the Rust span of each token.
//!
//! The body's tokens are laid back out as text: two tokens that touch in the
//! invoking file touch in the text, any others are one space apart, so the DSL
//! lexer sees what the author wrote (`#fff`, `12px`, `a::b`) and not the token
//! stream's own spacing. Every token remembers its byte range in that text, so a
//! diagnostic's offset maps back to the Rust token it points at.

use proc_macro::{Delimiter, Span, TokenStream, TokenTree};
use viso_dsl::syntax::TextSize;

pub struct SourceText {
    pub text: String,
    /// `(start, end, span)` per token, ascending and disjoint.
    tokens: Vec<(u32, u32, Span)>,
}

impl SourceText {
    pub fn new(body: TokenStream) -> Self {
        let mut source = Self {
            text: String::new(),
            tokens: Vec::new(),
        };
        let mut last = None;
        source.push_stream(body, &mut last);
        source
    }

    fn push_stream(&mut self, stream: TokenStream, last: &mut Option<Span>) {
        for tree in stream {
            match tree {
                TokenTree::Group(group) => {
                    let (open, close) = match group.delimiter() {
                        Delimiter::Brace => ("{", "}"),
                        Delimiter::Bracket => ("[", "]"),
                        Delimiter::Parenthesis => ("(", ")"),
                        Delimiter::None => ("", ""),
                    };
                    if !open.is_empty() {
                        self.push(open, group.span_open(), last);
                    }
                    self.push_stream(group.stream(), last);
                    if !close.is_empty() {
                        self.push(close, group.span_close(), last);
                    }
                }
                TokenTree::Ident(ident) => self.push(&ident.to_string(), ident.span(), last),
                TokenTree::Punct(punct) => {
                    let mut utf8 = [0; 4];
                    self.push(punct.as_char().encode_utf8(&mut utf8), punct.span(), last)
                }
                TokenTree::Literal(literal) => {
                    self.push(&literal.to_string(), literal.span(), last)
                }
            }
        }
    }

    fn push(&mut self, token: &str, span: Span, last: &mut Option<Span>) {
        if let Some(previous) = *last
            && !touches(previous, span)
        {
            self.text.push(' ');
        }
        let start = self.text.len() as u32;
        self.text.push_str(token);
        self.tokens.push((start, self.text.len() as u32, span));
        *last = Some(span);
    }

    /// The span of the token at `offset`, else of the first token after it, else
    /// of the last token.
    pub fn span_at(&self, offset: TextSize) -> proc_macro2::Span {
        let offset = offset.to_u32();
        let after = self.tokens.partition_point(|&(_, end, _)| end <= offset);
        self.tokens
            .get(after)
            .or_else(|| self.tokens.last())
            .map_or_else(proc_macro2::Span::call_site, |&(_, _, span)| span.into())
    }
}

fn touches(previous: Span, next: Span) -> bool {
    let (end, start) = (previous.end(), next.start());
    end.line() == start.line() && end.column() == start.column()
}
