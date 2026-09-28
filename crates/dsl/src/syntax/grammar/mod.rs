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

mod decl;
mod expr;
mod incremental;
mod patterns;
mod stmt;
mod types;
mod view;

use std::cell::Cell;
use std::rc::Rc;

use super::cst::{GreenBuilder, GreenNode, GreenToken};
use super::kind::SyntaxKind;
use super::span::{TextRange, TextSize};
use super::token::Token;

use crate::diag::{Applicability, Diagnostic, Fix, TextEdit};

pub use super::parser::{Parse, ParseErrorKind};
pub use incremental::IncrementalParse;

/// Parses `tokens` (the full stream, trivia and the trailing [`SyntaxKind::Eof`]
/// included) over `source` into a lossless typed CST rooted at
/// [`SyntaxKind::CompilationUnit`] — the `.vs` file / `view!` entry.
pub fn parse(tokens: &[Token], source: &str) -> Parse {
    parse_entry(tokens, source, Entry::CompilationUnit)
}

/// Parses `tokens` over `source` as a single bare expression, rooted at
/// [`SyntaxKind::ExprStmt`]. This is not one of the three DSL source forms; it is
/// the fragment entry a formatter/REPL or a `computed`/property-value editing path
/// parses an isolated expression through, and the direct way to test the
/// expression grammar in isolation.
pub fn parse_expr(tokens: &[Token], source: &str) -> Parse {
    parse_entry(tokens, source, Entry::Expr)
}

/// The DSL entry productions. The three source forms (AGENTS 21.5) route through
/// one grammar so `ui!` / `component!` / `view!` share resolution downstream;
/// [`Entry::Expr`] is a fragment entry for an isolated expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// A `.vs` file or `view!("...")`: `ImportDecl* TopLevelDecl* EOF`.
    CompilationUnit,
    /// `ui! { ... }`: a bare view fragment (`ViewStructureItem* EOF`).
    ViewFragment,
    /// `component! { ... }`: `ImportDecl* ComponentDecl EOF`.
    ComponentEntry,
    /// A single bare expression fragment (not a source form): `Expr EOF`.
    Expr,
}

/// Parses `tokens` over `source` using the given [`Entry`] production.
pub fn parse_entry(tokens: &[Token], source: &str, entry: Entry) -> Parse {
    let mut parser = Parser::new(tokens, source);
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
    /// Consumes the next significant token into the current node, retagged as
    /// `kind` when set: a recognized contextual keyword becomes its keyword kind
    /// and a keyword in a label position becomes `Ident` (§12.4–§12.5).
    ///
    /// `len` takes only that many bytes of the token and leaves the rest as the
    /// next significant token: a `>>`, `>=` or `>>=` split so a generic list can
    /// close on its first `>`.
    Token {
        kind: Option<SyntaxKind>,
        len: Option<u32>,
    },
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
    /// The parser's cursor, lookahead high-water mark and error count when the
    /// node opened, recorded for [`incremental`] reparse units.
    opened: (usize, usize, usize),
    /// Guards against forgetting to complete/abandon a marker in debug builds.
    completed: bool,
}

impl Marker {
    fn new(pos: usize, opened: (usize, usize, usize)) -> Marker {
        Marker {
            pos,
            opened,
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
        if incremental::is_unit(kind) {
            let (start, lookahead, errors) = self.opened;
            p.units.push(UnitRecord {
                kind,
                start,
                end: p.pos,
                lookahead,
                errors: errors..p.errors.len(),
            });
        }
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
struct Parser<'t, 's> {
    /// Every token (trivia included), used only for kind/text lookups by index.
    tokens: &'t [Token],
    /// The original source, so a production can peek at a token's text where the
    /// grammar keys on a context word (e.g. the callable keywords `Fn`/`FnMut`).
    source: &'s str,
    /// Indices into `tokens` of the significant (non-trivia, non-Eof) tokens, in
    /// order. The parser's cursor `pos` indexes *this* list.
    significant: Vec<usize>,
    /// Cursor into `significant`.
    pos: usize,
    /// Bytes of the token at the cursor already consumed by a `>` split. While
    /// non-zero the cursor sees only the token's remainder.
    split: u32,
    events: Vec<Event>,
    errors: Vec<Diagnostic>,
    /// One past the farthest significant index any lookahead has inspected; a
    /// value past `significant.len()` means the end of input was inspected.
    lookahead: Cell<usize>,
    /// Every completed reparse unit, in completion order.
    units: Vec<UnitRecord>,
}

/// What [`incremental`] reparse needs to know about one completed unit node,
/// in significant-token indices of the parser that produced it.
#[derive(Debug, Clone)]
struct UnitRecord {
    kind: SyntaxKind,
    /// The cursor when the node opened and when it closed.
    start: usize,
    end: usize,
    /// The lookahead high-water mark when the node opened: everything the
    /// parse so far depended on.
    lookahead: usize,
    /// The errors emitted between opening and closing the node.
    errors: std::ops::Range<usize>,
}

impl<'t, 's> Parser<'t, 's> {
    fn new(tokens: &'t [Token], source: &'s str) -> Parser<'t, 's> {
        let significant = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.kind.is_trivia() && t.kind != SyntaxKind::Eof)
            .map(|(i, _)| i)
            .collect();
        Parser {
            tokens,
            source,
            significant,
            pos: 0,
            split: 0,
            events: Vec::new(),
            errors: Vec::new(),
            lookahead: Cell::new(0),
            units: Vec::new(),
        }
    }

    fn finish(self) -> (Vec<Event>, Vec<Diagnostic>) {
        (self.events, self.errors)
    }

    // --- Cursor over significant tokens -----------------------------------

    /// The kind of the significant token `n` positions ahead of the cursor, or
    /// [`SyntaxKind::Eof`] past the end.
    fn nth(&self, n: usize) -> SyntaxKind {
        self.saw(n);
        let kind = self
            .significant
            .get(self.pos + n)
            .map_or(SyntaxKind::Eof, |&i| self.tokens[i].kind);
        if n == 0 && self.split > 0 {
            split_rest(kind, self.split)
        } else {
            kind
        }
    }

    /// Raises the lookahead high-water mark to cover the token `n` ahead.
    fn saw(&self, n: usize) {
        self.lookahead
            .set(self.lookahead.get().max(self.pos + n + 1));
    }

    /// The kind at the cursor.
    fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    /// The source text of the significant token `n` positions ahead of the
    /// cursor, or `""` past the end. Used only where the grammar keys on a
    /// context word (callable keywords, `empty`) that lexes as a bare
    /// identifier — never on the hot path.
    fn token_text(&self, n: usize) -> &'s str {
        self.saw(n);
        self.significant.get(self.pos + n).map_or("", |&i| {
            let r = self.tokens[i].range;
            let skip = if n == 0 { self.split } else { 0 };
            &self.source[(r.start().to_u32() + skip) as usize..r.end().to_u32() as usize]
        })
    }

    /// The contextual keyword the token `n` ahead spells, if it is a plain
    /// identifier whose text is one (§12.3). Whether it *acts* as a keyword is
    /// the caller's §12.4 lookahead decision.
    fn nth_contextual(&self, n: usize) -> Option<SyntaxKind> {
        if self.nth(n) != SyntaxKind::Ident {
            return None;
        }
        SyntaxKind::contextual_keyword(self.token_text(n))
    }

    /// Whether the token `n` ahead spells the contextual keyword `kw`.
    fn nth_at_contextual(&self, n: usize, kw: SyntaxKind) -> bool {
        self.nth_contextual(n) == Some(kw)
    }

    /// Whether the cursor spells the contextual keyword `kw`.
    fn at_contextual(&self, kw: SyntaxKind) -> bool {
        self.nth_at_contextual(0, kw)
    }

    /// Whether the token `n` ahead can name something: an identifier, a raw
    /// identifier, or (since they lex as identifiers) a contextual keyword.
    fn nth_is_ident(&self, n: usize) -> bool {
        matches!(self.nth(n), SyntaxKind::Ident | SyntaxKind::RawIdent)
    }

    /// Whether the cursor is at end of significant input.
    fn at_end(&self) -> bool {
        self.saw(0);
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
            Some(&i) => self.tokens[i].range.start() + TextSize::from(self.split),
            None => self.tokens.last().map_or(TextSize::ZERO, |t| t.range.end()),
        }
    }

    /// Consumes the current significant token into the tree.
    fn bump_any(&mut self) {
        let kind = (self.split > 0).then(|| self.current());
        self.bump_token(kind);
    }

    /// Consumes the current significant token, recording it in the tree as
    /// `kind` rather than its lexed kind.
    fn bump_as(&mut self, kind: SyntaxKind) {
        self.bump_token(Some(kind));
    }

    fn bump_token(&mut self, kind: Option<SyntaxKind>) {
        if self.at_end() {
            return;
        }
        self.events.push(Event::Token { kind, len: None });
        self.pos += 1;
        self.split = 0;
    }

    /// Whether the cursor starts with a `>`: a `>` itself, or a `>>`, `>=` or
    /// `>>=` whose first byte can close a generic list.
    fn at_gt(&self) -> bool {
        matches!(
            self.current(),
            SyntaxKind::Gt | SyntaxKind::Shr | SyntaxKind::Ge | SyntaxKind::ShrEq
        )
    }

    /// Consumes one `>` closing a generic list, splitting a longer token that
    /// starts with `>` so its remainder stays for the next production.
    fn eat_gt(&mut self) -> bool {
        match self.current() {
            SyntaxKind::Gt => {
                self.bump_any();
                true
            }
            SyntaxKind::Shr | SyntaxKind::Ge | SyntaxKind::ShrEq => {
                self.events.push(Event::Token {
                    kind: Some(SyntaxKind::Gt),
                    len: Some(1),
                });
                self.split += 1;
                true
            }
            _ => false,
        }
    }

    /// [`Parser::eat_gt`], or a missing-token error.
    fn expect_gt(&mut self) {
        if !self.eat_gt() {
            self.error(ParseErrorKind::MissingToken);
        }
    }

    /// Consumes the contextual keyword `kw` if the cursor spells it, retagging
    /// it as `kw`.
    fn eat_contextual(&mut self, kw: SyntaxKind) -> bool {
        if self.at_contextual(kw) {
            self.bump_as(kw);
            true
        } else {
            false
        }
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

    /// Consumes `kind`, or records an error, keeping the tree shape the grammar
    /// expects. A closing delimiter missing at end of input is an unclosed
    /// delimiter (E1401); any other absence is a missing token (E1404).
    fn expect(&mut self, kind: SyntaxKind) -> bool {
        if self.eat(kind) {
            return true;
        }
        let closer = matches!(
            kind,
            SyntaxKind::RBrace | SyntaxKind::RParen | SyntaxKind::RBracket
        );
        if closer && self.at_end() {
            self.error(ParseErrorKind::UnclosedDelimiter);
        } else {
            self.error(ParseErrorKind::MissingToken);
        }
        false
    }

    // --- Markers ----------------------------------------------------------

    /// Opens a node at the cursor, to be completed or abandoned later.
    fn start(&mut self) -> Marker {
        let pos = self.events.len();
        self.events.push(Event::Start {
            kind: TOMBSTONE,
            forward_parent: None,
        });
        Marker::new(pos, (self.pos, self.lookahead.get(), self.errors.len()))
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

    /// The kind a completed node was given.
    fn kind_of(&self, c: CompletedMarker) -> SyntaxKind {
        match self.events[c.pos] {
            Event::Start { kind, .. } => kind,
            _ => unreachable!("a completed marker points at a Start event"),
        }
    }

    // --- Errors -----------------------------------------------------------

    /// Records a structural error at the current offset.
    fn error(&mut self, kind: ParseErrorKind) {
        let at = self.offset();
        self.errors.push(kind.to_diagnostic(TextRange::new(at, at)));
    }

    /// The cursor position, for a repetition loop's progress check.
    fn cursor(&self) -> (usize, u32) {
        (self.pos, self.split)
    }

    /// Guarantees a repetition loop advances: if the item production consumed
    /// nothing since `before`, the current token becomes an error node (E1403)
    /// so the loop cannot spin on input no item can start with.
    fn ensure_progress(&mut self, before: (usize, u32)) {
        if self.cursor() == before && !self.at_end() {
            self.err_and_bump(ParseErrorKind::UnexpectedTokens);
        }
    }

    /// Flags the reserved `child` at the cursor and drops just the word, so the
    /// node that follows parses as an ordinary anonymous node. When a type
    /// follows, deleting the word and the whitespace after it is the fix.
    fn reserved_child(&mut self) {
        let at = self.significant[self.pos];
        let range = self.tokens[at].range;
        let mut diagnostic = ParseErrorKind::ChildReserved
            .to_diagnostic(range)
            .expecting(["anonymous node", "node <name>: <Component>"], "child");
        if matches!(self.nth(1), SyntaxKind::Ident | SyntaxKind::RawIdent) {
            let end = self.tokens[at + 1..]
                .iter()
                .find(|t| t.kind != SyntaxKind::Whitespace)
                .map_or(range.end(), |t| t.range.start());
            diagnostic.fixes.push(Fix {
                title: "remove `child`".to_string(),
                applicability: Applicability::MachineApplicable,
                edits: vec![TextEdit::new(TextRange::new(range.start(), end), "")],
            });
        }
        let m = self.start();
        self.errors.push(diagnostic);
        self.bump_any();
        m.complete(self, SyntaxKind::ErrorNode);
    }

    /// Wraps the current token in an `ErrorNode` and advances, so recovery always
    /// makes progress and the token still lands in the tree.
    /// A stray closing delimiter reported as unexpected is an unmatched closer
    /// (E1402).
    fn err_and_bump(&mut self, kind: ParseErrorKind) {
        let kind = match (kind, self.current()) {
            (
                ParseErrorKind::UnexpectedTokens,
                SyntaxKind::RBrace | SyntaxKind::RParen | SyntaxKind::RBracket,
            ) => ParseErrorKind::UnmatchedCloser,
            _ => kind,
        };
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
            Entry::Expr => {
                self.expr_fragment();
                SyntaxKind::ExprStmt
            }
        };
        m.complete(self, root_kind);
    }

    /// `.vs` / `view!`: `ImportDecl* TopLevelDecl* EOF` — imports followed by
    /// top-level declarations.
    fn compilation_unit(&mut self) {
        decl::compilation_unit(self);
    }

    /// `ui!`: a bare view fragment — `ViewStructureItem* EOF` with no surrounding
    /// `view { }` wrapper.
    fn view_fragment(&mut self) {
        view::view_fragment_items(self);
    }

    /// `component!`: `ImportDecl* ComponentDecl EOF` — imports followed by a single
    /// component declaration.
    fn component_entry(&mut self) {
        decl::component_entry(self);
    }

    /// A single bare expression fragment. Any tokens past the expression are wrapped
    /// as an `ErrorNode` so recovery stays total and the tree stays lossless even
    /// on garbage input.
    fn expr_fragment(&mut self) {
        if expr::at_expr_start(self) {
            expr::expr(self);
        }
        while !self.at_end() {
            self.err_and_bump(ParseErrorKind::UnexpectedTokens);
        }
    }
}

/// Plays the event stream into a [`GreenBuilder`], resolving `forward_parent`
/// chains and interleaving trivia from the raw token stream so the result is
/// lossless.
fn build_tree(tokens: &[Token], source: &str, events: Vec<Event>) -> Rc<GreenNode> {
    let (root, raw) = play_events(tokens, source, events);
    debug_assert!(
        raw_covers_all(tokens, raw),
        "build_tree left tokens unconsumed"
    );
    root
}

/// [`build_tree`] without the whole-stream check: plays `events` over a token
/// stream that may continue past them, returning the tree and the raw cursor.
fn play_events(tokens: &[Token], source: &str, mut events: Vec<Event>) -> (Rc<GreenNode>, usize) {
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
    // Bytes of `tokens[raw]` already emitted by a split `Token` event.
    let mut raw_off = 0u32;
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
            Event::Token { kind, len } => {
                if raw_off == 0 {
                    emit_trivia(&mut builder, tokens, source, &mut raw);
                }
                emit_next_significant(
                    &mut builder,
                    tokens,
                    source,
                    (&mut raw, &mut raw_off),
                    (kind, len),
                );
            }
        }
    }
    (builder.finish(), raw)
}

/// Emits trivia at the raw cursor and then the next significant token into the
/// builder, advancing past all of them. The significant token takes `retag` as
/// its kind when set; a `len` emits only that many bytes of it (a `>` split),
/// tracked in `raw_off` until the token's last piece is emitted.
fn emit_next_significant(
    builder: &mut GreenBuilder,
    tokens: &[Token],
    source: &str,
    (raw, raw_off): (&mut usize, &mut u32),
    (retag, len): (Option<SyntaxKind>, Option<u32>),
) {
    while *raw < tokens.len() {
        let mut t = tokens[*raw];
        if t.kind == SyntaxKind::Eof {
            *raw += 1;
            continue;
        }
        if t.kind.is_trivia() {
            builder.token_from(t, source);
            *raw += 1;
            continue;
        }
        if let Some(kind) = retag {
            t.kind = kind;
        }
        let start = t.range.start().to_u32() + *raw_off;
        let end = t.range.end().to_u32();
        match len {
            Some(n) => {
                let text = &source[start as usize..(start + n) as usize];
                builder.token(GreenToken::new(t.kind, text));
                *raw_off += n;
            }
            None if *raw_off > 0 => {
                builder.token(GreenToken::new(
                    t.kind,
                    &source[start as usize..end as usize],
                ));
                *raw_off = 0;
                *raw += 1;
            }
            None => {
                builder.token_from(t, source);
                *raw += 1;
            }
        }
        break;
    }
}

/// The kind of what remains of a `>`-led token after `consumed` bytes were split
/// off as `>`.
fn split_rest(kind: SyntaxKind, consumed: u32) -> SyntaxKind {
    match (kind, consumed) {
        (SyntaxKind::Shr, 1) => SyntaxKind::Gt,
        (SyntaxKind::ShrEq, 1) => SyntaxKind::Ge,
        (SyntaxKind::ShrEq, 2) | (SyntaxKind::Ge, 1) => SyntaxKind::Eq,
        _ => kind,
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

/// A binding or declaration name (§12.5): an identifier, a raw identifier or a
/// contextual keyword. A strict keyword here is E1301; it is still consumed as
/// the name so the declaration keeps its shape.
fn name(p: &mut Parser) {
    if p.nth_is_ident(0) {
        p.bump_any();
    } else if p.current().is_strict_keyword() {
        p.error(ParseErrorKind::ReservedIdent);
        p.bump_as(SyntaxKind::Ident);
    } else {
        p.error(ParseErrorKind::MissingToken);
    }
}

/// Whether the cursor is at a label (§12.5): any identifier or keyword.
fn at_label(p: &Parser) -> bool {
    p.nth_is_ident(0) || p.current().is_keyword()
}

/// A label (§12.5): member names, `::` path segments, field and variant names,
/// named-argument and attribute labels. Any identifier or keyword is accepted
/// and a keyword is recorded as `Ident`.
fn label(p: &mut Parser) {
    if p.at(SyntaxKind::RawIdent) {
        p.bump_any();
    } else if at_label(p) {
        p.bump_as(SyntaxKind::Ident);
    } else {
        p.error(ParseErrorKind::MissingToken);
    }
}

/// `Attribute*` — any run of `@path(args)` attributes preceding a declaration,
/// member, node item or statement, each wrapped in its own node.
fn attributes(p: &mut Parser) {
    while p.at(SyntaxKind::At) {
        let m = p.start();
        p.bump_any(); // `@`
        expr::path_only(p);
        if p.at(SyntaxKind::LParen) {
            expr::arg_list(p);
        }
        m.complete(p, SyntaxKind::Attribute);
    }
}
