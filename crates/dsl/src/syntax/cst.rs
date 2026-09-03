//! The lossless concrete syntax tree: an immutable, shareable rowan-style
//! **green tree** built from the flat token stream.
//!
//! A green node is offset-relative and reference-counted: it stores its total
//! text length (the sum of its children) rather than an absolute span, so an
//! identical subtree can be shared and the tree can be spliced during an
//! incremental reparse without renumbering every offset. Absolute [`TextRange`]s
//! are recovered by walking from the root, accumulating child lengths. This is
//! the rust-analyzer design; a compiler/editor CST is a cold-path structure, so
//! the `Rc` sharing here is deliberate and allowed.
//!
//! The tree is **lossless**: every token — including whitespace, comments, and
//! error tokens — becomes a green token, so `root.text()` reconstructs the
//! source byte-for-byte. Slice K builds a single flat [`SyntaxKind::Root`] whose
//! children are all the lexed tokens; the grammar-driven node structure lands in
//! the next slice, but the round-trip contract holds already.
//!
//! The tree also models the two recovery shapes the parser needs: an
//! [`SyntaxKind::ErrorNode`] wraps tokens that did not fit a production, and a
//! [`SyntaxKind::MissingToken`] is a zero-width green token standing in for a
//! required token the source omitted.

use std::rc::Rc;

use super::kind::SyntaxKind;
use super::span::{TextRange, TextSize};
use super::token::{LexError, Token};

/// A child of a green node: either a subtree or a leaf token.
#[derive(Debug, Clone)]
pub enum GreenChild {
    /// A nested node, reference-counted so identical subtrees can be shared.
    Node(Rc<GreenNode>),
    /// A leaf token (including trivia and error tokens).
    Token(GreenToken),
}

impl GreenChild {
    /// The child's total text length in bytes.
    #[inline]
    pub fn text_len(&self) -> TextSize {
        match self {
            GreenChild::Node(n) => n.text_len(),
            GreenChild::Token(t) => t.text_len(),
        }
    }

    /// The child's syntax kind.
    #[inline]
    pub fn kind(&self) -> SyntaxKind {
        match self {
            GreenChild::Node(n) => n.kind(),
            GreenChild::Token(t) => t.kind(),
        }
    }

    /// Appends this child's source text to `out`.
    fn write_text(&self, out: &mut String) {
        match self {
            GreenChild::Node(n) => n.write_text(out),
            GreenChild::Token(t) => out.push_str(&t.text),
        }
    }
}

/// An interior node: a kind and its ordered children. Immutable once built.
#[derive(Debug, Clone)]
pub struct GreenNode {
    kind: SyntaxKind,
    /// Sum of all children's text lengths — an offset-relative length, so the
    /// node carries no absolute position and can be shared or spliced.
    text_len: TextSize,
    children: Vec<GreenChild>,
}

impl GreenNode {
    /// Builds a node from `kind` and `children`, computing `text_len` as the sum
    /// of the children's lengths.
    pub fn new(kind: SyntaxKind, children: Vec<GreenChild>) -> GreenNode {
        debug_assert!(kind.is_node(), "GreenNode kind must be a node kind");
        let mut text_len = TextSize::ZERO;
        for child in &children {
            text_len = text_len + child.text_len();
        }
        GreenNode {
            kind,
            text_len,
            children,
        }
    }

    /// The node's kind.
    #[inline]
    pub fn kind(&self) -> SyntaxKind {
        self.kind
    }

    /// The node's total text length in bytes.
    #[inline]
    pub fn text_len(&self) -> TextSize {
        self.text_len
    }

    /// The node's children in source order.
    #[inline]
    pub fn children(&self) -> &[GreenChild] {
        &self.children
    }

    /// Reconstructs this node's source text into a fresh `String`.
    ///
    /// For the root this equals the original source byte-for-byte (the
    /// losslessness contract).
    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.text_len.to_usize());
        self.write_text(&mut out);
        out
    }

    /// Appends this node's source text to `out`.
    fn write_text(&self, out: &mut String) {
        for child in &self.children {
            child.write_text(out);
        }
    }

    /// Collects the lexical errors carried by tokens anywhere in this subtree,
    /// paired with their absolute range, by walking with a running offset. Used
    /// by tests and the diagnostics layer.
    pub fn errors(&self) -> Vec<(TextRange, LexError)> {
        let mut out = Vec::new();
        self.collect_errors(TextSize::ZERO, &mut out);
        out
    }

    fn collect_errors(&self, start: TextSize, out: &mut Vec<(TextRange, LexError)>) {
        let mut offset = start;
        for child in &self.children {
            match child {
                GreenChild::Node(n) => n.collect_errors(offset, out),
                GreenChild::Token(t) => {
                    if let Some(e) = t.error {
                        out.push((TextRange::new(offset, offset + t.text_len()), e));
                    }
                }
            }
            offset = offset + child.text_len();
        }
    }
}

/// A leaf token in the green tree. Owns its text so a subtree is self-contained
/// and shareable across reparses without referring back to the source buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GreenToken {
    kind: SyntaxKind,
    text: String,
    /// A recoverable lexical error the lexer attached to this token, if any.
    error: Option<LexError>,
}

impl GreenToken {
    /// A green token of `kind` spelled `text`, with no error.
    pub fn new(kind: SyntaxKind, text: impl Into<String>) -> GreenToken {
        GreenToken {
            kind,
            text: text.into(),
            error: None,
        }
    }

    /// A green token carrying a recoverable lexical `error`.
    pub fn with_error(kind: SyntaxKind, text: impl Into<String>, error: LexError) -> GreenToken {
        GreenToken {
            kind,
            text: text.into(),
            error: Some(error),
        }
    }

    /// A zero-width [`SyntaxKind::MissingToken`] placeholder for a required token
    /// the source omitted. Its empty text keeps the tree lossless (it contributes
    /// nothing to `text()`) while giving a node the child slot it expects.
    pub fn missing() -> GreenToken {
        GreenToken {
            kind: SyntaxKind::MissingToken,
            text: String::new(),
            error: None,
        }
    }

    /// The token's kind.
    #[inline]
    pub fn kind(&self) -> SyntaxKind {
        self.kind
    }

    /// The token's source spelling.
    #[inline]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The token's text length in bytes.
    #[inline]
    pub fn text_len(&self) -> TextSize {
        TextSize::new(self.text.len() as u32)
    }

    /// The recoverable lexical error on this token, if any.
    #[inline]
    pub fn error(&self) -> Option<LexError> {
        self.error
    }
}

/// A builder for a green tree: a small stack machine driven by
/// `start_node` / `token` / `finish_node`, the standard rowan builder shape.
///
/// The parser calls `start_node(kind)` to open a node, `token(...)` to push
/// leaves, and `finish_node()` to close the innermost open node (folding it into
/// its parent's child list). `finish()` closes the last open node and returns the
/// root. Nodes are constructed bottom-up so each is immutable once finished.
#[derive(Debug, Default)]
pub struct GreenBuilder {
    /// One entry per open node: its kind and the children accumulated so far.
    stack: Vec<(SyntaxKind, Vec<GreenChild>)>,
    /// The finished root, set by `finish`.
    root: Option<Rc<GreenNode>>,
}

impl GreenBuilder {
    /// A fresh builder with no open nodes.
    pub fn new() -> GreenBuilder {
        GreenBuilder {
            stack: Vec::new(),
            root: None,
        }
    }

    /// Opens a new node of `kind`; subsequent tokens/nodes attach to it until the
    /// matching `finish_node`.
    pub fn start_node(&mut self, kind: SyntaxKind) {
        debug_assert!(kind.is_node(), "start_node kind must be a node kind");
        self.stack.push((kind, Vec::new()));
    }

    /// Pushes a leaf token into the innermost open node.
    pub fn token(&mut self, token: GreenToken) {
        self.push_child(GreenChild::Token(token));
    }

    /// Pushes a leaf token built from a lexer [`Token`] and the `source` it was
    /// lexed from, copying out its spelling.
    pub fn token_from(&mut self, token: Token, source: &str) {
        let text = token.text(source);
        let green = match token.error {
            Some(e) => GreenToken::with_error(token.kind, text, e),
            None => GreenToken::new(token.kind, text),
        };
        self.token(green);
    }

    /// Pushes a zero-width [`SyntaxKind::MissingToken`] into the innermost node.
    pub fn missing_token(&mut self) {
        self.token(GreenToken::missing());
    }

    /// Closes the innermost open node, folding it into its parent (or making it
    /// the root if it was the outermost).
    pub fn finish_node(&mut self) {
        let (kind, children) = self.stack.pop().expect("finish_node with no open node");
        let node = Rc::new(GreenNode::new(kind, children));
        if self.stack.is_empty() {
            self.root = Some(node);
        } else {
            self.push_child(GreenChild::Node(node));
        }
    }

    /// Finishes the tree and returns the root. All opened nodes must be closed.
    pub fn finish(mut self) -> Rc<GreenNode> {
        debug_assert!(self.stack.is_empty(), "finish with open nodes remaining");
        self.root.take().expect("finish with no root node")
    }

    /// Pushes a child into the innermost open node, or as the root child list if
    /// none is open (a defensive path; callers open a root first).
    fn push_child(&mut self, child: GreenChild) {
        self.stack
            .last_mut()
            .expect("token pushed with no open node")
            .1
            .push(child);
    }
}

/// Builds a flat lossless CST from a full token stream: a single
/// [`SyntaxKind::Root`] whose children are every token in order (trivia and
/// errors included, the final [`SyntaxKind::Eof`] dropped since it is empty).
///
/// This is the Slice K primitive: it proves the green layer round-trips
/// (`root.text() == source`) before any grammar exists. The grammar-driven node
/// structure replaces this flat wrap in the next slice.
pub fn flat_tree(tokens: &[Token], source: &str) -> Rc<GreenNode> {
    let mut builder = GreenBuilder::new();
    builder.start_node(SyntaxKind::Root);
    for &token in tokens {
        if token.kind == SyntaxKind::Eof {
            continue;
        }
        builder.token_from(token, source);
    }
    builder.finish_node();
    builder.finish()
}
