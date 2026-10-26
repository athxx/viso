//! The `.vs` tokenizer: source text to a flat, lossless token stream, every
//! malformed span carrying a [`LexError`] rather than stopping it.
//!
//! [`SyntaxKind`] tags tokens and the compiler's tree nodes alike; a
//! [`Token`] is a kind, a byte [`TextRange`] and an optional error. The
//! compiler parses from this stream, and a code editor highlights with it.

pub mod kind;
pub mod lexer;
pub mod span;
pub mod token;

pub use kind::SyntaxKind;
pub use lexer::{LexState, Lexer, tokenize};
pub use span::{LineCol, LineIndex, TextRange, TextSize};
pub use token::{LexError, Token};
