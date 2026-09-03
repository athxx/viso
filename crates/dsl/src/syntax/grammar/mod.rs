//! The typed grammar parser: an event-driven recursive-descent + Pratt parser
//! that turns the flat token stream into a lossless, typed green tree.
//!
//! This supersedes the coarse skeleton parser (Slice K). It keeps the same two
//! contracts — **losslessness** (`root.text() == source`, trivia and all) and
//! **recovery** (no input panics or stops the parser at the first error) — but
//! produces the grammar's real node kinds (Appendix A) instead of coarse
//! `Item`/`Block` grouping.
//!
//! ## Why an event buffer
//!
//! A Pratt expression parser must, after parsing a left operand, retroactively
//! wrap it in a `BinaryExpr` when it discovers a following operator. A pure
//! stack-machine [`GreenBuilder`] cannot re-open an already-finished node, so the
//! parser here emits a flat list of [`Event`]s (`Start`/`Finish`/`Token`) and can
//! *precede* an earlier `Start` with a new one via a [`Marker`]. A single final
//! pass ([`build_tree`]) plays the events into a [`GreenBuilder`], interleaving
//! trivia in their original stream position so the tree stays lossless.
//!
//! ## Trivia handling
//!
//! The parser drives over *significant* tokens only (whitespace/comments are
//! filtered into [`Parser::significant`]). Trivia are re-attached during tree
//! construction: [`build_tree`] walks the raw token stream in lockstep with the
//! events and emits each trivia token in place, so no whitespace or comment is
//! lost and none changes the grammatical structure.

mod expr;
mod patterns;
mod types;

use std::rc::Rc;

use super::cst::{GreenBuilder, GreenNode};
use super::kind::SyntaxKind;
use super::span::{TextRange, TextSize};
use super::token::Token;

pub use super::parser::{Parse, ParseError, ParseErrorKind};

/// Parses `tokens` (the full stream, trivia and the trailing [`SyntaxKind::Eof`]
/// included) over `source` into a lossless typed CST rooted at
/// [`SyntaxKind::CompilationUnit`] — the `.vs` file / `view!` entry.
pub fn parse(tokens: &[Token], source: &str) -> Parse {
    parse_entry(tokens, source, Entry::CompilationUnit)
}

/// The three DSL entry productions (AGENTS 21.5). All route through one grammar
/// so `ui!` / `component!` / `view!` share resolution downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// A `.vs` file or `view!("...")`: `ImportDecl* TopLevelDecl* EOF`.
    CompilationUnit,
    /// `ui! { ... }`: a bare view fragment (`ViewStructureItem* EOF`).
    ViewFragment,
    /// `component! { ... }`: `ImportDecl* ComponentDecl EOF`.
    ComponentEntry,
}

/// Parses `tokens` over `source` using the given [`Entry`] production.
pub fn parse_entry(tokens: &[Token], source: &str, entry: Entry) -> Parse {
    let mut parser = Parser::new(tokens);
    parser.parse_entry(entry);
    let (events, errors) = parser.finish();
    let root = build_tree(tokens, source, events);
    Parse { root, errors }
}

/// One step in the flat parse event stream, played back by [`build_tree`].
#[derive(Debug, Clone)]
enum Event {
    /// Opens a node of `kind`. A placeholder `kind` of [`TOMBSTONE`] is an
    /// abandoned marker that [`build_tree`] skips.
    Start {
        kind: SyntaxKind,
        /// If set, this `Start` is *forwarded* to precede the `Start` at the
        /// given event index, so the earlier node becomes a child of this one.
        /// This is how a Pratt parser wraps an already-parsed left operand.
        forward_parent: Option<usize>,
    },
    /// Closes the innermost open node.
    Finish,
    /// Consumes `n_significant`-th significant token into the current node.
    Token,
}

/// The placeholder kind of an abandoned [`Marker`]: its `Start`/`Finish` produce
/// no node.
const TOMBSTONE: SyntaxKind = SyntaxKind::MissingToken;

/// A position in the event stream that a node was (or will be) opened at.
///
/// Returned by [`Parser::start`]. Complete it with [`Marker::complete`] to set the
/// node's kind, or drop the work with [`Marker::abandon`]. A completed marker
/// yields a [`CompletedMarker`] that a later `start` can *precede* — the
/// mechanism a Pratt parser uses to wrap its left operand.
#[must_use]
struct Marker {
    /// Index of this marker's `Start` event.
    pos: usize,
    /// Guards against forgetting to complete/abandon a marker in debug builds.
    completed: bool,
}

impl Marker {
    fn new(pos: usize) -> Marker {
        Marker {
            pos,
            completed: false,
        }
    }

    /// Sets this node's kind and closes it, returning a handle that a later
    /// [`Parser::start_at`] can wrap.
    fn complete(mut self, p: &mut Parser, kind: SyntaxKind) -> CompletedMarker {
        self.completed = true;
        match &mut p.events[self.pos] {
            Event::Start { kind: k, .. } => *k = kind,
            _ => unreachable!("marker must point at a Start event"),
        }
        p.events.push(Event::Finish);
        CompletedMarker { pos: self.pos }
    }

    /// Discards this marker: the `Start` becomes a tombstone that produces no
    /// node. Any tokens consumed between start and abandon stay in the parent.
    fn abandon(mut self, p: &mut Parser) {
        self.completed = true;
        // Only a trailing, empty marker can be cheaply popped; otherwise leave a
        // tombstone `Start` that `build_tree` ignores.
        if self.pos == p.events.len() - 1 {
            match p.events.pop() {
                Some(Event::Start {
                    kind: TOMBSTONE,
                    forward_parent: None,
                }) => {}
                _ => unreachable!("abandon of a non-trailing or completed marker"),
            }
        }
    }
}

impl Drop for Marker {
    fn drop(&mut self) {
        debug_assert!(
            self.completed,
            "a Marker was neither completed nor abandoned"
        );
    }
}

/// A completed node's position, so a later [`Parser::start_at`] can insert a new
/// parent `Start` just before it.
#[derive(Clone, Copy)]
struct CompletedMarker {
    pos: usize,
}

/// The event-driven parser state. Drives over significant tokens; trivia are
/// re-attached at tree-build time.
struct Parser<'t> {
    /// Every token (trivia included), used only for kind/text lookups by index.
    tokens: &'t [Token],
    /// Indices into `tokens` of the significant (non-trivia, non-Eof) tokens, in
    /// order. The parser's cursor `pos` indexes *this* list.
    significant: Vec<usize>,
    /// Cursor into `significant`.
    pos: usize,
    events: Vec<Event>,
    errors: Vec<ParseError>,
}

impl<'t> Parser<'t> {
    fn new(tokens: &'t [Token]) -> Parser<'t> {
        let significant = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.kind.is_trivia() && t.kind != SyntaxKind::Eof)
            .map(|(i, _)| i)
            .collect();
        Parser {
            tokens,
            significant,
            pos: 0,
            events: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn finish(self) -> (Vec<Event>, Vec<ParseError>) {
        (self.events, self.errors)
    }

    // --- Cursor over significant tokens -----------------------------------

    /// The kind of the significant token `n` positions ahead of the cursor, or
    /// [`SyntaxKind::Eof`] past the end.
    fn nth(&self, n: usize) -> SyntaxKind {
        self.significant
            .get(self.pos + n)
            .map_or(SyntaxKind::Eof, |&i| self.tokens[i].kind)
    }

    /// The kind at the cursor.
    fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    /// Whether the cursor is at end of significant input.
    fn at_end(&self) -> bool {
        self.pos >= self.significant.len()
    }

    /// Whether the cursor is at a token of `kind`.
    fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    /// The byte offset of the significant token at the cursor (end of source at
    /// the end).
    fn offset(&self) -> TextSize {
        match self.significant.get(self.pos) {
            Some(&i) => self.tokens[i].range.start(),
            None => self.tokens.last().map_or(TextSize::ZERO, |t| t.range.end()),
        }
    }

    /// Consumes the current significant token into the tree.
    fn bump_any(&mut self) {
        if self.at_end() {
            return;
        }
        self.events.push(Event::Token);
        self.pos += 1;
    }

    /// Consumes the current token if it is `kind`, returning whether it was.
    fn eat(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.bump_any();
            true
        } else {
            false
        }
    }

    /// Consumes `kind`, or records an error and inserts a `MissingToken` node in
    /// its place, keeping the tree shape the grammar expects.
    fn expect(&mut self, kind: SyntaxKind) -> bool {
        if self.eat(kind) {
            true
        } else {
            self.error(ParseErrorKind::MissingToken);
            false
        }
    }

    // --- Markers ----------------------------------------------------------

    /// Opens a node at the cursor, to be completed or abandoned later.
    fn start(&mut self) -> Marker {
        let pos = self.events.len();
        self.events.push(Event::Start {
            kind: TOMBSTONE,
            forward_parent: None,
        });
        Marker::new(pos)
    }

    /// Opens a node that *precedes* an already-completed node `c`, so `c` becomes
    /// its first child. This wraps a Pratt left operand in a binary/postfix node.
    fn start_at(&mut self, c: CompletedMarker) -> Marker {
        let m = self.start();
        match &mut self.events[c.pos] {
            Event::Start { forward_parent, .. } => *forward_parent = Some(m.pos),
            _ => unreachable!("start_at target must be a Start event"),
        }
        m
    }

    // --- Errors -----------------------------------------------------------

    /// Records a structural error at the current offset.
    fn error(&mut self, kind: ParseErrorKind) {
        let at = self.offset();
        self.errors.push(ParseError {
            range: TextRange::new(at, at),
            kind,
        });
    }

    /// Wraps the current token in an `ErrorNode` and advances, so recovery always
    /// makes progress and the token still lands in the tree.
    fn err_and_bump(&mut self, kind: ParseErrorKind) {
        let m = self.start();
        self.error(kind);
        self.bump_any();
        m.complete(self, SyntaxKind::ErrorNode);
    }

    // --- Entry -------------------------------------------------------------

    /// Parses the chosen entry production, wrapping the whole input in its root
    /// node so every significant token lands under it.
    fn parse_entry(&mut self, entry: Entry) {
        let m = self.start();
        let root_kind = match entry {
            Entry::CompilationUnit => {
                self.compilation_unit();
                SyntaxKind::CompilationUnit
            }
            Entry::ViewFragment => {
                self.view_fragment();
                SyntaxKind::ViewFragment
            }
            Entry::ComponentEntry => {
                self.component_entry();
                SyntaxKind::ComponentEntry
            }
        };
        m.complete(self, root_kind);
    }

    /// `.vs` / `view!`: a run of top-level items until end of input. The typed
    /// declaration grammar lands in the next commit; for now every top-level
    /// construct is parsed as an expression statement or recovered, so the
    /// expression parser and its tests stand on their own.
    fn compilation_unit(&mut self) {
        while !self.at_end() {
            self.top_level_item();
        }
    }

    /// `ui!`: a bare view fragment — same placeholder body as the compilation
    /// unit until the view grammar lands next commit.
    fn view_fragment(&mut self) {
        while !self.at_end() {
            self.top_level_item();
        }
    }

    /// `component!`: a single component declaration — placeholder until the
    /// declaration grammar lands next commit.
    fn component_entry(&mut self) {
        while !self.at_end() {
            self.top_level_item();
        }
    }

    /// A single top-level construct. This is a **placeholder** body for commit 1:
    /// it parses an expression (so the expression parser is exercised end to end)
    /// terminated by an optional `;`, and recovers on anything it cannot start.
    /// Commit 2 replaces this with the real declaration/view grammar.
    fn top_level_item(&mut self) {
        if expr::at_expr_start(self) {
            let m = self.start();
            expr::expr(self);
            self.eat(SyntaxKind::Semi);
            m.complete(self, SyntaxKind::ExprStmt);
        } else {
            self.err_and_bump(ParseErrorKind::UnexpectedTokens);
        }
    }
}

/// Plays the event stream into a [`GreenBuilder`], resolving `forward_parent`
/// chains and interleaving trivia from the raw token stream so the result is
/// lossless.
fn build_tree(tokens: &[Token], source: &str, mut events: Vec<Event>) -> Rc<GreenNode> {
    let mut builder = GreenBuilder::new();
    // Cursor over the raw token stream, so trivia are emitted in place.
    let mut raw = 0usize;

    // Resolve forwarded parents: a `Start` with `forward_parent` must be emitted
    // *before* the node it forwards to. We rewrite the stream by moving each
    // forwarded `Start` to just before its target, following the chain. This is
    // the rust-analyzer approach, done in a scratch buffer.
    //
    // When a `Start` at index `i` forwards to a parent at `fp`, the parent's own
    // `Start`/`Finish` pair already exists in the stream; only *where the parent
    // opens* moves. So each parent hoisted here leaves a tombstone behind at its
    // original slot (skipped at playback), while its original `Finish` stays put
    // and still balances the hoisted `Start`. Replacing the hoisted slot with a
    // `Finish` instead would inject an unbalanced close — the source of the
    // flattened trees and "no open node" panics this replaces.
    let tombstone = Event::Start {
        kind: TOMBSTONE,
        forward_parent: None,
    };
    let mut forwarded: Vec<SyntaxKind> = Vec::new();
    let mut ordered: Vec<Event> = Vec::with_capacity(events.len());
    for i in 0..events.len() {
        match std::mem::replace(&mut events[i], tombstone.clone()) {
            Event::Start {
                kind,
                mut forward_parent,
            } => {
                // Collect this node's kind plus any chain of parents that forward
                // into it, so the outermost parent opens first.
                forwarded.clear();
                forwarded.push(kind);
                while let Some(fp) = forward_parent {
                    match std::mem::replace(&mut events[fp], tombstone.clone()) {
                        Event::Start {
                            kind: k,
                            forward_parent: next,
                        } => {
                            forwarded.push(k);
                            forward_parent = next;
                        }
                        _ => unreachable!("forward_parent must point at a Start"),
                    }
                }
                for &k in forwarded.iter().rev() {
                    ordered.push(Event::Start {
                        kind: k,
                        forward_parent: None,
                    });
                }
            }
            other => ordered.push(other),
        }
    }

    // Tombstones open no node and have no `Finish` of their own — an abandoned
    // marker never pushed one, and a hoisted parent's `Finish` still sits with
    // its real (relocated) `Start`. So tombstones are ignored entirely and each
    // `Finish` pairs (LIFO) with the innermost real open node. `depth` tracks the
    // open real nodes: trailing trivia after the final significant token has no
    // later event to trigger its lazy flush, so it is flushed into the root just
    // before the outermost real `Finish` returns the depth to zero.
    let mut depth = 0usize;
    for event in ordered {
        match event {
            Event::Start {
                kind: TOMBSTONE, ..
            } => {}
            Event::Start { kind, .. } => {
                // Attach any pending trivia *before* opening the node so leading
                // whitespace/comments sit inside its parent, next to the token
                // they lead. The outermost (root) node has no parent yet, so its
                // leading trivia must instead land *inside* it — open the root
                // first and let the following token flush that trivia into it.
                if depth > 0 {
                    emit_trivia(&mut builder, tokens, source, &mut raw);
                }
                builder.start_node(kind);
                depth += 1;
            }
            Event::Finish => {
                depth -= 1;
                if depth == 0 {
                    // Closing the root: flush the source's trailing trivia so no
                    // whitespace or comment past the last token is lost.
                    emit_trivia(&mut builder, tokens, source, &mut raw);
                }
                builder.finish_node();
            }
            Event::Token => {
                emit_trivia(&mut builder, tokens, source, &mut raw);
                emit_next_significant(&mut builder, tokens, source, &mut raw);
            }
        }
    }
    let root = builder.finish();
    debug_assert!(
        raw_covers_all(tokens, raw),
        "build_tree left tokens unconsumed"
    );
    root
}

/// Emits trivia at the raw cursor and then the next significant token into the
/// builder, advancing past all of them.
fn emit_next_significant(
    builder: &mut GreenBuilder,
    tokens: &[Token],
    source: &str,
    raw: &mut usize,
) {
    while *raw < tokens.len() {
        let t = tokens[*raw];
        if t.kind == SyntaxKind::Eof {
            *raw += 1;
            continue;
        }
        let is_trivia = t.kind.is_trivia();
        builder.token_from(t, source);
        *raw += 1;
        if !is_trivia {
            break;
        }
    }
}

/// Emits every trivia token at the raw cursor into the builder, advancing past
/// them, until the next significant (or Eof) token.
fn emit_trivia(builder: &mut GreenBuilder, tokens: &[Token], source: &str, raw: &mut usize) {
    while *raw < tokens.len() {
        let t = tokens[*raw];
        if t.kind == SyntaxKind::Eof {
            *raw += 1;
            continue;
        }
        if t.kind.is_trivia() {
            builder.token_from(t, source);
            *raw += 1;
        } else {
            break;
        }
    }
}

/// Debug check that the raw cursor consumed the whole token stream.
fn raw_covers_all(tokens: &[Token], raw: usize) -> bool {
    tokens[raw..].iter().all(|t| t.kind == SyntaxKind::Eof)
}
