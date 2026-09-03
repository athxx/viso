//! Typed AST — a set of rust-analyzer-style *typed views* projected over the
//! green/red tree, not a separately owned tree.
//!
//! The parser (see [`crate::syntax`]) produces a lossless green tree; the AST layer
//! adds zero tree duplication on top of it. Every wrapper here is a newtype over a
//! [`SyntaxNode`](crate::syntax::SyntaxNode) plus a kind check ([`AstNode::cast`]),
//! and its accessors ([`support`]) filter that node's direct children. The green
//! tree stays the single source of truth a formatter, LSP, goto-definition, or
//! rename all navigate, so the AST costs only a kind-tag comparison to project.
//!
//! Only the Core authoring surface parsed in Slice L gets typed wrappers; Advanced
//! declarations parse to [`SyntaxKind::AdvancedItem`](crate::syntax::SyntaxKind::AdvancedItem)
//! and are reachable as raw syntax, resolved when their consumer lands.

mod nodes;
mod support;

pub use nodes::{
    ActionDecl, AdvancedItem, AnonymousNode, AstNode, BinaryExpr, Block, CallExpr, CastExpr,
    ClosureExpr, CompilationUnit, ComponentDecl, ComponentEntry, ComputedDecl, ConstDecl, EnumDecl,
    EnumVariant, EventDecl, EventHandler, ExportDecl, Expr, FieldExpr, FillClause, FnDecl, IfExpr,
    ImportDecl, ImportItem, IndexExpr, InputDecl, Item, ListExpr, LiteralExpr, MatchExpr, Member,
    ModulePath, NamedNode, NodeBody, NodeMember, Param, ParamList, ParenExpr, PathExpr,
    PropertyBinding, PropertyPath, RangeExpr, RecordDecl, RecordExpr, RecordField, RenameClause,
    ReturnType, StateDecl, SystemDecl, TaskDecl, TryExpr, TupleExpr, TwoWayBinding, TypeAliasDecl,
    TypePath, UnaryExpr, ViewBlock, ViewDecl, ViewFor, ViewFragment, ViewIf, ViewItem, ViewMatch,
    ViewMatchArm,
};
