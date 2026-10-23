//! The calls a view reads and pauses the game its root runs through:
//! `game.<query>()` reads a system's `@query` function by name,
//! `game.is_paused()` and `game.tick()` the game's clock, and `game.pause()`
//! and `game.resume()` are the actions of `viso::game::session`. Anything
//! else on `game` is `E2001`; a view of a package without a system has no
//! game to read.

use viso_behavior::game::{PAUSE_PATH, RESUME_PATH};
use viso_schema::view::GameQuery;

use super::InferCx;
use crate::ast::{AstNode, Expr, FieldExpr, PathExpr};
use crate::diag::Diagnostic;
use crate::hir::ty::Ty;
use crate::resolve::Resolution;
use crate::resolve::suggest::{Candidate, attach, nearest};
use crate::syntax::{SyntaxNode, SyntaxToken, TextRange};

/// A system's `@query` function, as a view's `game.<name>()` reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct GameQueryDecl {
    /// The function's name, which the view calls it by.
    pub name: String,
    /// The system declaring it.
    pub system: String,
    /// What it returns.
    pub ret: Ty,
    /// The span of the function's name.
    pub at: TextRange,
}

/// The standard queries and actions every game has, by name.
const STANDARD: [&str; 4] = ["is_paused", "tick", "pause", "resume"];

impl InferCx<'_> {
    /// The game query the call at `range` reads.
    pub(crate) fn game_call(&self, range: TextRange) -> Option<&GameQuery> {
        self.game_calls.get(&range)
    }

    /// Types `game.name(args)`, the call `node`.
    pub(super) fn game_method_call(
        &mut self,
        name: &SyntaxToken,
        args: &[Expr],
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Ty {
        let text = name.text();
        let text = text.trim_start_matches("r#");
        if !self.env.runs_game() {
            self.diagnostics.push(Diagnostic::error(
                "E2001",
                name.text_range(),
                "`game` reads the game a view's root runs, and the package declares no \
                 `system`",
            ));
            self.infer_args_alone(args);
            return Ty::Unknown;
        }
        if !args.is_empty() {
            self.diagnostics.push(Diagnostic::error(
                "E2103",
                node.text_range(),
                format!("`game.{text}` takes no arguments"),
            ));
            self.infer_args_alone(args);
        }
        let (query, ty) = match text {
            "is_paused" => (GameQuery::Paused, Ty::Bool),
            "tick" => (GameQuery::Tick, Ty::I64),
            "pause" | "resume" => {
                let path = if text == "pause" {
                    PAUSE_PATH
                } else {
                    RESUME_PATH
                };
                if let Some(entry) = self.env.natives().and_then(|n| n.function(path)) {
                    self.record_native(node.text_range(), entry.id, false);
                }
                return self.check_against(Ty::Unit, expected, node);
            }
            _ => match self.env.game_queries().iter().find(|q| q.name == text) {
                Some(decl) => (
                    GameQuery::System {
                        system: decl.system.as_str().into(),
                        function: decl.name.as_str().into(),
                    },
                    decl.ret.clone(),
                ),
                None => {
                    self.unknown_query(name, text);
                    return Ty::Unknown;
                }
            },
        };
        self.game_calls.insert(node.text_range(), query);
        self.check_against(ty, expected, node)
    }

    /// `E2001` for `game.text`, which names no query, with the nearest names.
    fn unknown_query(&mut self, name: &SyntaxToken, text: &str) {
        let mut diagnostic = Diagnostic::error(
            "E2001",
            name.text_range(),
            format!(
                "the game has no query `{text}`: a view reads a system's `@query fn` by \
                 name, or `is_paused`, `tick`, `pause` and `resume`"
            ),
        );
        let queries = self.env.game_queries();
        let names = queries
            .iter()
            .map(|q| q.name.as_str())
            .chain(STANDARD)
            .map(|name| Candidate {
                name,
                declared_at: None,
            });
        let suggestions = nearest(text, names);
        attach(&mut diagnostic, name.text_range(), &suggestions);
        self.diagnostics.push(diagnostic);
    }

    /// The method name of `callee`, a field access, when its receiver is the
    /// bare `game`.
    pub(super) fn game_receiver(&self, callee: &SyntaxNode) -> Option<SyntaxToken> {
        let field = FieldExpr::cast(callee.clone())?;
        let path = PathExpr::cast(field.receiver()?.syntax().clone())?;
        let mut segments = path.segments();
        let head = segments.next()?;
        if segments.next().is_some() || self.refs.get(&head.text_range()) != Some(&Resolution::Game)
        {
            return None;
        }
        field.field()
    }

    /// `E2001` for `game` read as a value rather than through a call.
    pub(super) fn bare_game(&mut self, at: TextRange) -> Ty {
        self.diagnostics.push(Diagnostic::error(
            "E2001",
            at,
            "`game` is not a value: read it through a call, `game.is_paused()` or a \
             system's `@query fn`",
        ));
        Ty::Unknown
    }
}
