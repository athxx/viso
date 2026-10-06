//! Incremental reparse. After one edit, only the innermost component/system
//! member or top-level item enclosing the edit is re-lexed and re-parsed, and the
//! new subtree is spliced into the old green tree. Every node the edit does not
//! touch, each sibling member included, is reused by pointer.
//!
//! The result is identical to a full parse of the edited source. The parser's
//! only state is its cursor, so a production is a pure function of the token
//! stream from where it starts. That makes a unit reparsable in place when:
//! - everything the parse had looked at when the unit opened lies before the
//!   first token the edit changed, so the prefix parses exactly as before and
//!   reaches the unit with the same cursor;
//! - the re-lexed unit ends on its old end boundary, so the suffix tokens are
//!   the old ones shifted;
//! - the unit's production consumes exactly the re-lexed tokens, so the suffix
//!   parse resumes at the same cursor.
//!
//! Anything else falls back to a full parse, so correctness never depends on the
//! fast path being taken.

use std::ops::Range;
use std::rc::Rc;

use super::super::cst::{GreenChild, GreenNode};
use super::super::kind::SyntaxKind;
use super::super::lexer::{LexState, Lexer, tokenize};
use super::super::reparse::Edit;
use super::super::span::{TextRange, TextSize};
use super::super::token::Token;
use super::{Entry, Parse, Parser, decl, play_events};
use crate::diag::Diagnostic;

/// Members of a component or system body.
const MEMBER_UNITS: &[SyntaxKind] = &[
    SyntaxKind::InputDecl,
    SyntaxKind::StateDecl,
    SyntaxKind::ComputedDecl,
    SyntaxKind::EventDecl,
    SyntaxKind::SlotDecl,
    SyntaxKind::EffectDecl,
    SyntaxKind::ViewDecl,
    SyntaxKind::ConstDecl,
    SyntaxKind::FnDecl,
    SyntaxKind::ActionDecl,
    SyntaxKind::TaskDecl,
    SyntaxKind::NativeDecl,
    SyntaxKind::AdvancedItem,
];

/// Items of a compilation unit.
const TOP_LEVEL_UNITS: &[SyntaxKind] = &[
    SyntaxKind::ImportDecl,
    SyntaxKind::ExportDecl,
    SyntaxKind::ComponentDecl,
    SyntaxKind::SystemDecl,
    SyntaxKind::RecordDecl,
    SyntaxKind::EnumDecl,
    SyntaxKind::TypeAliasDecl,
    SyntaxKind::ConstDecl,
    SyntaxKind::FnDecl,
    SyntaxKind::ActionDecl,
    SyntaxKind::TaskDecl,
    SyntaxKind::ShaderDecl,
    SyntaxKind::NativeDecl,
    SyntaxKind::TraitDecl,
    SyntaxKind::ImplDecl,
    SyntaxKind::AdvancedItem,
];

/// Whether the parser records nodes of `kind` as reparse units.
pub(super) fn is_unit(kind: SyntaxKind) -> bool {
    MEMBER_UNITS.contains(&kind) || TOP_LEVEL_UNITS.contains(&kind)
}

/// A parse kept current across edits: the token stream, the tree and its
/// errors, and what the parser recorded about each reparse unit.
#[derive(Debug, Clone)]
pub struct IncrementalParse {
    entry: Entry,
    tokens: Vec<Token>,
    parse: Parse,
    units: Vec<Unit>,
}

/// A reparse unit in source offsets.
#[derive(Debug, Clone)]
struct Unit {
    kind: SyntaxKind,
    range: TextRange,
    /// The end of the farthest token the parse had inspected when the unit
    /// opened; `None` if it had inspected the end of input. After an in-place
    /// reparse a later unit keeps a bound that may exceed the exact value (the
    /// old unit's lookahead cannot be subtracted out); an overestimate only
    /// costs reuse, never correctness.
    lookahead: Option<TextSize>,
    /// The unit's own errors, as indices into the parse's error list.
    errors: Range<usize>,
}
// Units are kept in completion order: by end, an inner unit before the unit
// that encloses it.

/// The production a unit is reparsed with.
#[derive(Clone, Copy)]
enum Production {
    Member,
    Item,
    AttributedItem,
}

impl IncrementalParse {
    /// Lexes and parses `source` from scratch with the `entry` production.
    pub fn new(source: &str, entry: Entry) -> IncrementalParse {
        let tokens = tokenize(source);
        let mut parser = Parser::new(&tokens, source);
        parser.parse_entry(entry);
        let units = convert(&parser, TextSize::ZERO, 0);
        let (events, errors) = parser.finish();
        let (root, _) = play_events(&tokens, source, events);
        IncrementalParse {
            entry,
            parse: Parse { root, errors },
            tokens,
            units,
        }
    }

    /// The current tree and its parse errors.
    pub fn parse(&self) -> &Parse {
        &self.parse
    }

    /// The current token stream, trivia and the final `Eof` included.
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    /// Applies `edit`, where `new_source` is the edited text. Returns whether a
    /// single unit was reparsed in place; `false` means a full reparse.
    pub fn edit(&mut self, edit: &Edit, new_source: &str) -> bool {
        match self.reparse_in_place(edit, new_source) {
            Some(next) => {
                *self = next;
                true
            }
            None => {
                *self = IncrementalParse::new(new_source, self.entry);
                false
            }
        }
    }

    fn reparse_in_place(&self, edit: &Edit, new_source: &str) -> Option<IncrementalParse> {
        let old_len = self.parse.root.text_len().to_usize();
        let removed = edit.range.as_usize();
        if removed.end > old_len || old_len - removed.len() + edit.insert.len() != new_source.len()
        {
            return None;
        }

        // The path of nodes whose interior strictly contains the edit, each with
        // its start offset and the index of the child the path continues into.
        let mut path: Vec<(&Rc<GreenNode>, TextSize, usize)> = Vec::new();
        let (mut node, mut start) = (&self.parse.root, TextSize::ZERO);
        loop {
            let mut offset = start;
            let mut inner = None;
            for (i, child) in node.children().iter().enumerate() {
                let end = offset + child.text_len();
                if offset < edit.range.start() && edit.range.end() < end {
                    if let GreenChild::Node(n) = child {
                        inner = Some((i, n, offset));
                    }
                    break;
                }
                if end > edit.range.start() {
                    break;
                }
                offset = end;
            }
            let Some((i, child, child_start)) = inner else {
                break;
            };
            path.push((node, start, i));
            node = child;
            start = child_start;
        }

        (0..path.len())
            .rev()
            .find_map(|depth| self.reparse_unit(&path, depth, edit, new_source))
    }

    /// Reparses the child at `path[depth]` if it is a unit that can be reparsed
    /// in place.
    fn reparse_unit(
        &self,
        path: &[(&Rc<GreenNode>, TextSize, usize)],
        depth: usize,
        edit: &Edit,
        new_source: &str,
    ) -> Option<IncrementalParse> {
        let (parent, parent_start, index) = path[depth];
        let GreenChild::Node(old) = &parent.children()[index] else {
            return None;
        };
        let production = match parent.kind() {
            SyntaxKind::ComponentDecl | SyntaxKind::SystemDecl
                if MEMBER_UNITS.contains(&old.kind()) =>
            {
                Production::Member
            }
            SyntaxKind::CompilationUnit if TOP_LEVEL_UNITS.contains(&old.kind()) => {
                let attributed = parent.children()[..index]
                    .iter()
                    .rev()
                    .find(|c| !c.kind().is_trivia())
                    .is_some_and(|c| c.kind() == SyntaxKind::Attribute);
                if attributed {
                    Production::AttributedItem
                } else {
                    Production::Item
                }
            }
            _ => return None,
        };
        let old_start = parent_start
            + parent.children()[..index]
                .iter()
                .map(GreenChild::text_len)
                .fold(TextSize::ZERO, |a, b| a + b);
        let old_range = TextRange::new(old_start, old_start + old.text_len());
        let unit = self
            .units
            .iter()
            .position(|u| u.kind == old.kind() && u.range == old_range)?;
        let record = &self.units[unit];

        // The unit's old token span, and a clean token boundary before it.
        let first = self.token_at(old_range.start())?;
        let after = self.token_at(old_range.end())?;
        if first > 0 {
            let before = self.tokens[first - 1].kind;
            let separated = before.is_trivia()
                || matches!(
                    before,
                    SyntaxKind::LBrace
                        | SyntaxKind::RBrace
                        | SyntaxKind::Semi
                        | SyntaxKind::RParen
                        | SyntaxKind::RBracket
                        | SyntaxKind::Comma
                );
            if !separated {
                return None;
            }
        }

        // Re-lex the unit; it must end exactly on the shifted old end boundary.
        let delta = edit.insert.len() as i64 - edit.range.len().to_u32() as i64;
        let new_end = shift(old_range.end(), delta);
        let mut lexer = Lexer::resume(new_source, old_range.start(), LexState);
        let mut lexed = Vec::new();
        loop {
            let token = lexer.next_token();
            if token.kind == SyntaxKind::Eof || token.range.end() > new_end {
                return None;
            }
            lexed.push(token);
            if token.range.end() == new_end {
                break;
            }
        }

        // Everything the parse had inspected when the unit opened must precede
        // the first token the edit changed.
        let old_span = &self.tokens[first..after];
        let unchanged = old_span
            .iter()
            .zip(&lexed)
            .take_while(|(a, b)| a == b && b.range.end() <= edit.range.start())
            .count();
        let changed_at = old_span
            .get(unchanged)
            .map_or(old_range.end(), |t| t.range.start());
        if record.lookahead.is_none_or(|end| end > changed_at) {
            return None;
        }

        let mut tokens = Vec::with_capacity(first + lexed.len() + self.tokens.len() - after);
        tokens.extend_from_slice(&self.tokens[..first]);
        tokens.extend_from_slice(&lexed);
        tokens.extend(self.tokens[after..].iter().map(|t| Token {
            range: shift_range(t.range, delta),
            ..*t
        }));

        // Parse the unit's production alone over the rest of the stream.
        let region = &tokens[first..];
        let mut parser = Parser::new(region, new_source);
        let root = parser.start();
        match production {
            Production::Member => decl::member(&mut parser),
            Production::Item => decl::compilation_unit_item(&mut parser),
            Production::AttributedItem => decl::top_level_decl(&mut parser),
        }
        root.complete(&mut parser, SyntaxKind::Root);
        let significant = lexed
            .iter()
            .filter(|t| !t.kind.is_trivia() && t.kind != SyntaxKind::Eof)
            .count();
        if parser.pos != significant || parser.split != 0 {
            return None;
        }
        // The unit's own record must hold every error the production emitted.
        let top = parser.units.last()?;
        if top.start != 0 || top.end != significant || top.errors != (0..parser.errors.len()) {
            return None;
        }
        let floor = record.lookahead?;
        let region_units = convert(&parser, floor, record.errors.start);
        let region_lookahead =
            lookahead_end(&parser, parser.lookahead.get()).map(|end| end.max(floor));
        let (events, region_errors) = parser.finish();
        let (tree, _) = play_events(region, new_source, events);
        let mut nodes = tree.children().iter().filter(|c| !c.kind().is_trivia());
        let (Some(GreenChild::Node(fresh)), None) = (nodes.next(), nodes.next()) else {
            return None;
        };
        if old_start + fresh.text_len() != new_end {
            return None;
        }

        let old_errors = record.errors.clone();
        let errors_delta = region_errors.len() as i64 - old_errors.len() as i64;
        let mut errors = Vec::with_capacity(self.parse.errors.len());
        errors.extend_from_slice(&self.parse.errors[..old_errors.start]);
        errors.extend(region_errors);
        errors.extend(
            self.parse.errors[old_errors.end..]
                .iter()
                .map(|e| shift_diagnostic(e, delta)),
        );

        let mut units = Vec::with_capacity(self.units.len() + region_units.len());
        for u in &self.units {
            if u.range.start() >= old_range.start() && u.range.end() <= old_range.end() {
                continue;
            }
            let mut u = u.clone();
            if u.range.start() >= old_range.end() {
                u.range = shift_range(u.range, delta);
                u.lookahead = u
                    .lookahead
                    .map(|end| shift(end, delta))
                    .zip(region_lookahead)
                    .map(|(end, region)| end.max(region));
                u.errors = shift_index(u.errors.start, errors_delta)
                    ..shift_index(u.errors.end, errors_delta);
            } else if u.range.end() >= old_range.end() {
                u.range = TextRange::new(u.range.start(), shift(u.range.end(), delta));
                u.errors.end = shift_index(u.errors.end, errors_delta);
            }
            units.push(u);
        }
        units.extend(region_units);
        units.sort_by_key(|u| (u.range.end(), std::cmp::Reverse(u.range.start())));

        // Rebuild the ancestors, cloning every untouched sibling pointer.
        let mut replacement = fresh.clone();
        for &(ancestor, _, child) in path[..=depth].iter().rev() {
            let mut children = ancestor.children().to_vec();
            children[child] = GreenChild::Node(replacement);
            replacement = Rc::new(GreenNode::new(ancestor.kind(), children));
        }

        Some(IncrementalParse {
            entry: self.entry,
            tokens,
            parse: Parse {
                root: replacement,
                errors,
            },
            units,
        })
    }

    /// The index of the token starting at `offset`, if one does.
    fn token_at(&self, offset: TextSize) -> Option<usize> {
        self.tokens
            .binary_search_by_key(&offset, |t| t.range.start())
            .ok()
    }
}

/// Converts the units `parser` recorded from its significant-token indices to
/// source offsets. `floor` is the lookahead already reached before the parser
/// started and `error_base` the index its first error takes in the whole parse.
fn convert(parser: &Parser, floor: TextSize, error_base: usize) -> Vec<Unit> {
    parser
        .units
        .iter()
        .filter(|r| r.end > r.start)
        .map(|r| {
            let start = parser.tokens[parser.significant[r.start]].range.start();
            let end = parser.tokens[parser.significant[r.end - 1]].range.end();
            Unit {
                kind: r.kind,
                range: TextRange::new(start, end),
                lookahead: lookahead_end(parser, r.lookahead).map(|own| own.max(floor)),
                errors: r.errors.start + error_base..r.errors.end + error_base,
            }
        })
        .collect()
}

/// The end of the farthest token a lookahead high-water mark covers, or `None`
/// if it covers the end of input.
fn lookahead_end(parser: &Parser, high: usize) -> Option<TextSize> {
    match high {
        0 => Some(TextSize::ZERO),
        n if n > parser.significant.len() => None,
        n => Some(parser.tokens[parser.significant[n - 1]].range.end()),
    }
}

fn shift(offset: TextSize, delta: i64) -> TextSize {
    TextSize::new((offset.to_u32() as i64 + delta) as u32)
}

fn shift_range(range: TextRange, delta: i64) -> TextRange {
    TextRange::new(shift(range.start(), delta), shift(range.end(), delta))
}

fn shift_index(index: usize, delta: i64) -> usize {
    (index as i64 + delta) as usize
}

fn shift_diagnostic(diagnostic: &Diagnostic, delta: i64) -> Diagnostic {
    let mut shifted = diagnostic.clone();
    shifted.primary = shift_range(shifted.primary, delta);
    // Only spans in this file move; another module's file is not being edited.
    for related in shifted.related.iter_mut().filter(|r| r.module.is_none()) {
        related.range = shift_range(related.range, delta);
    }
    for fix in &mut shifted.fixes {
        for edit in fix.edits.iter_mut().filter(|e| e.module.is_none()) {
            edit.range = shift_range(edit.range, delta);
        }
    }
    shifted
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCES: &[&str] = &[
        "import a::b;\n@doc(\"x\")\ncomponent C {\n    input title: String = \"t\";\n    state count = 0;\n    computed d: I64 = count * 2;\n    action inc() { count += 1; }\n    view { Text { text: title; } }\n}\nfn f(x: I64) -> I64 { x + 1 }\n",
        "export record P { x: F32; y: F32 = 0.0; }\nenum S { idle; busy(I64); }\ntype Id = I64;\nconst N: I64 = 4;\n",
        "component C { state a = x < B; state b = 1; fn g() { let v = make::<T>(1); } }\n",
        "component C { state a = 1 state b = 2; event e(x: I64); }\nsystem Tick { fn run() { } }\n",
        "// lead\ncomponent C { /* c */ state s = \"a}b\"; @inline fn h() { } }\n",
        "native fn f(a: I64) -> I64;\nnative type T;\ncomponent C { native action g() requires { x::y }; }\n",
        "trait Shape: Eq { type Unit: Clone; const N: I64; fn area(self) -> F64; }\nimpl<T> Shape for Box<T> where T: Eq { type Unit = I64; const N: I64 = 1; fn area(self) -> F64 { 1.0 } }\nimpl P { fn new(mut self, x: I64) -> Self { self } }\n",
    ];

    const INSERTS: &[&str] = &["x", " ", "{", "}", ";", "\"", "1", "(", "//", "<", "\n"];

    fn assert_same(incremental: &IncrementalParse, source: &str, context: &dyn Fn() -> String) {
        let fresh = IncrementalParse::new(source, incremental.entry);
        assert_eq!(incremental.tokens, fresh.tokens, "tokens: {}", context());
        assert_eq!(
            format!("{:?}", incremental.parse.root),
            format!("{:?}", fresh.parse.root),
            "tree: {}",
            context()
        );
        assert_eq!(
            format!("{:?}", incremental.parse.errors),
            format!("{:?}", fresh.parse.errors),
            "errors: {}",
            context()
        );
        // Units match, except that a lookahead bound may be more conservative.
        let exact = |units: &[Unit]| -> Vec<_> {
            units
                .iter()
                .map(|u| (u.kind, u.range, u.errors.clone()))
                .collect()
        };
        assert_eq!(
            exact(&incremental.units),
            exact(&fresh.units),
            "units: {}",
            context()
        );
        for (a, b) in incremental.units.iter().zip(&fresh.units) {
            let conservative = match (a.lookahead, b.lookahead) {
                (None, _) => true,
                (Some(a), Some(b)) => a >= b,
                (Some(_), None) => false,
            };
            assert!(conservative, "lookahead {a:?} below {b:?}: {}", context());
        }
    }

    /// Every edit at every offset of every source, then a second edit on the
    /// result, matches a full parse of the edited text.
    #[test]
    fn incremental_reparse_matches_a_full_parse() {
        let mut reused = 0usize;
        let mut total = 0usize;
        for source in SOURCES {
            let offsets: Vec<usize> = (0..=source.len())
                .filter(|&i| source.is_char_boundary(i))
                .collect();
            for &at in &offsets {
                let at_size = TextSize::new(at as u32);
                let mut edits: Vec<(TextRange, &str)> = INSERTS
                    .iter()
                    .map(|&text| (TextRange::empty(at_size), text))
                    .collect();
                if let Some(next) = source[at..].chars().next() {
                    let end = TextSize::new((at + next.len_utf8()) as u32);
                    edits.push((TextRange::new(at_size, end), ""));
                }
                for (range, insert) in edits {
                    let edit = Edit::new(range, insert);
                    let edited = edit.apply(source);
                    let mut parse = IncrementalParse::new(source, Entry::CompilationUnit);
                    total += 1;
                    reused += usize::from(parse.edit(&edit, &edited));
                    assert_same(&parse, &edited, &|| format!("{edit:?} on {source:?}"));

                    // A second edit on the result exercises the maintained table.
                    let second_at = (at + edited.len() / 2) % edited.len().max(1);
                    if edited.is_char_boundary(second_at) {
                        let second =
                            Edit::new(TextRange::empty(TextSize::new(second_at as u32)), "7");
                        let twice = second.apply(&edited);
                        parse.edit(&second, &twice);
                        assert_same(&parse, &twice, &|| {
                            format!("{second:?} after {edit:?} on {source:?}")
                        });
                    }
                }
            }
        }
        assert!(
            reused * 4 > total,
            "only {reused} of {total} edits reparsed in place"
        );
    }

    #[test]
    fn an_edit_inside_one_member_reuses_every_sibling_member() {
        let source = "component C {\n    state a = 1;\n    state b = 2;\n    fn f() { }\n    view { Text { } }\n}\n";
        let mut parse = IncrementalParse::new(source, Entry::CompilationUnit);
        let component = |p: &IncrementalParse| match &p.parse().root.children()[0] {
            GreenChild::Node(n) => n.clone(),
            GreenChild::Token(_) => panic!("expected the component node"),
        };
        let before = component(&parse);
        let at = source.find("2;").unwrap();
        let edit = Edit::new(
            TextRange::new(TextSize::new(at as u32), TextSize::new(at as u32 + 1)),
            "20 + 1",
        );
        let edited = edit.apply(source);
        assert!(
            parse.edit(&edit, &edited),
            "the edit fell back to a full parse"
        );
        assert_eq!(parse.parse().root.text(), edited);
        let after = component(&parse);

        let members = |n: &GreenNode| -> Vec<(SyntaxKind, Rc<GreenNode>)> {
            n.children()
                .iter()
                .filter_map(|c| match c {
                    GreenChild::Node(n) => Some((n.kind(), n.clone())),
                    GreenChild::Token(_) => None,
                })
                .collect()
        };
        let (old, new) = (members(&before), members(&after));
        assert_eq!(old.len(), new.len());
        let mut replaced = 0;
        for ((kind, a), (_, b)) in old.iter().zip(&new) {
            if Rc::ptr_eq(a, b) {
                continue;
            }
            replaced += 1;
            assert_eq!(*kind, SyntaxKind::StateDecl);
            assert!(b.text().contains("20 + 1"));
        }
        assert_eq!(replaced, 1, "exactly the edited member is rebuilt");
    }

    #[test]
    fn an_edit_the_prefix_looked_ahead_into_falls_back() {
        // The item loop reads `fn` before `g`'s node opens, so rewriting that
        // keyword cannot reuse the old decision, and no unit encloses `g`.
        let source = "fn f() { }\nfn g() { }\n";
        let mut parse = IncrementalParse::new(source, Entry::CompilationUnit);
        let at = source.rfind("fn").unwrap() + 1;
        let edit = Edit::new(TextRange::empty(TextSize::new(at as u32)), "x");
        let edited = edit.apply(source);
        assert!(!parse.edit(&edit, &edited));
        assert_same(&parse, &edited, &|| edited.clone());
    }
}
