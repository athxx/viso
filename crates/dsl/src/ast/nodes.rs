//! Typed AST wrappers over the green tree.
//!
//! Following rust-analyzer's idiom, the AST is a set of *typed views*, not a
//! separate owned tree. Each wrapper is a newtype over a [`SyntaxNode`] plus a
//! kind check; [`AstNode::cast`] returns `Some` only when the node's kind matches,
//! and typed accessors filter that node's children (see [`support`](super::support)).
//! The green tree stays the single source of truth — the same tree a formatter, an
//! LSP, or a rename refactor walks — so projecting the AST costs nothing beyond a
//! kind tag comparison.
//!
//! Only the Core surface parsed in commits 1–2 gets wrappers here; Advanced items
//! parse to [`SyntaxKind::AdvancedItem`] and are reachable as raw syntax but carry
//! no typed view yet.

use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use super::support;

/// A typed view over a [`SyntaxNode`] of one specific [`SyntaxKind`] (or a fixed
/// set, for the expression/declaration enums).
///
/// `cast` is the sole constructor: it validates the node's kind and returns `None`
/// otherwise, so a wrapper always wraps a node of the right shape. `syntax` returns
/// the underlying node for navigation, ranges, and losslessness.
pub trait AstNode: Sized {
    /// Whether a node of `kind` can be cast to this wrapper.
    fn can_cast(kind: SyntaxKind) -> bool;

    /// Wraps `node` if its kind matches, else `None`.
    fn cast(node: SyntaxNode) -> Option<Self>;

    /// The underlying syntax node.
    fn syntax(&self) -> &SyntaxNode;
}

/// Declares a wrapper newtype over a single [`SyntaxKind`] plus its [`AstNode`]
/// impl. Accessors are added in a separate `impl` block below.
macro_rules! ast_node {
    ($(#[$m:meta])* $name:ident = $kind:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name {
            syntax: SyntaxNode,
        }

        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                kind == SyntaxKind::$kind
            }
            fn cast(node: SyntaxNode) -> Option<Self> {
                if Self::can_cast(node.kind()) {
                    Some($name { syntax: node })
                } else {
                    None
                }
            }
            fn syntax(&self) -> &SyntaxNode {
                &self.syntax
            }
        }
    };
}

// --- Roots -------------------------------------------------------------------

ast_node!(
    /// A `.vs` file / `view!` entry: imports then top-level declarations.
    CompilationUnit = CompilationUnit
);
ast_node!(
    /// A `ui!` bare view fragment: view items with no component/view wrapper.
    ViewFragment = ViewFragment
);
ast_node!(
    /// A `component!` entry: imports then one component declaration.
    ComponentEntry = ComponentEntry
);

impl CompilationUnit {
    /// The `import` declarations at the head of the unit.
    pub fn imports(&self) -> impl Iterator<Item = ImportDecl> {
        support::children(&self.syntax)
    }

    /// The top-level declarations, in source order. `export`-prefixed
    /// declarations appear as [`Item::Export`]; the wrapped declaration is
    /// reached through [`ExportDecl::declaration`].
    pub fn items(&self) -> impl Iterator<Item = Item> {
        support::children(&self.syntax)
    }
}

impl ViewFragment {
    /// The view structure items directly under the fragment.
    pub fn items(&self) -> impl Iterator<Item = ViewItem> {
        support::children(&self.syntax)
    }
}

impl ComponentEntry {
    /// The single component this entry declares, if it parsed.
    pub fn component(&self) -> Option<ComponentDecl> {
        support::child(&self.syntax)
    }
}

// --- Declarations ------------------------------------------------------------

ast_node!(
    /// `import ModulePath (as IDENT | ::{items})? ;`
    ImportDecl = ImportDecl
);
ast_node!(
    /// A `::`-separated module path.
    ModulePath = ModulePath
);
ast_node!(
    /// One `a` / `a as b` selective-import item inside `::{ ... }`.
    ImportItem = ImportItem
);
ast_node!(
    /// The `as IDENT` rename tail on an import or item.
    RenameClause = RenameClause
);
ast_node!(
    /// An `export`-prefixed declaration; visibility travels with the wrapped decl.
    ExportDecl = ExportDecl
);
ast_node!(
    /// `component IDENT ... { member* }`.
    ComponentDecl = ComponentDecl
);
ast_node!(
    /// `system IDENT ... { member* }`.
    SystemDecl = SystemDecl
);
ast_node!(
    /// `record IDENT ... { field* }`.
    RecordDecl = RecordDecl
);
ast_node!(
    /// One `IDENT : Type (= Expr)? ;` record field.
    RecordField = RecordField
);
ast_node!(
    /// `enum IDENT ... { variant* }`.
    EnumDecl = EnumDecl
);
ast_node!(
    /// One `IDENT VariantPayload? ;` enum variant.
    EnumVariant = EnumVariant
);
ast_node!(
    /// `input IDENT : Type (= Expr)? ;`.
    InputDecl = InputDecl
);
ast_node!(
    /// `state IDENT (: Type)? = Expr ;`.
    StateDecl = StateDecl
);
ast_node!(
    /// `computed IDENT (: Type)? = Expr ;`.
    ComputedDecl = ComputedDecl
);
ast_node!(
    /// `event IDENT ( EventParam* ) ;`.
    EventDecl = EventDecl
);
ast_node!(
    /// `const IDENT : Type = Expr ;`.
    ConstDecl = ConstDecl
);
ast_node!(
    /// `type IDENT GenericParams? = Type ;`.
    TypeAliasDecl = TypeAliasDecl
);
ast_node!(
    /// `fn IDENT ( ParamList ) ReturnType? ... Block`.
    FnDecl = FnDecl
);
ast_node!(
    /// `action IDENT ( ParamList ) ReturnType? Block`.
    ActionDecl = ActionDecl
);
ast_node!(
    /// `task IDENT ( ParamList ) ReturnType? Block`.
    TaskDecl = TaskDecl
);
ast_node!(
    /// `view ViewBlock` inside a component/system.
    ViewDecl = ViewDecl
);
ast_node!(
    /// An Advanced-tier declaration parsed to a placeholder (no resolution yet).
    AdvancedItem = AdvancedItem
);

impl ImportDecl {
    /// The imported module path.
    pub fn path(&self) -> Option<ModulePath> {
        support::child(&self.syntax)
    }

    /// The `as IDENT` rename, when the whole module is renamed.
    pub fn rename(&self) -> Option<RenameClause> {
        support::child(&self.syntax)
    }

    /// The selective `::{ a, b as c }` items, when present.
    pub fn items(&self) -> impl Iterator<Item = ImportItem> {
        support::children(&self.syntax)
    }
}

impl ModulePath {
    /// The path segments, each an identifier token, in order.
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.syntax
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
    }
}

impl ImportItem {
    /// The item's own name token.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The `as IDENT` rename on this item, if any.
    pub fn rename(&self) -> Option<RenameClause> {
        support::child(&self.syntax)
    }
}

impl RenameClause {
    /// The new name introduced by `as`.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }
}

impl ExportDecl {
    /// The declaration this `export` makes public.
    pub fn declaration(&self) -> Option<Item> {
        support::child(&self.syntax)
    }
}

impl ComponentDecl {
    /// The component's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The component's members (inputs, state, actions, view, ...), in order.
    pub fn members(&self) -> impl Iterator<Item = Member> {
        support::children(&self.syntax)
    }

    /// The component's `view` declaration, if it has one.
    pub fn view(&self) -> Option<ViewDecl> {
        support::child(&self.syntax)
    }
}

impl SystemDecl {
    /// The system's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The system's members, in order.
    pub fn members(&self) -> impl Iterator<Item = Member> {
        support::children(&self.syntax)
    }
}

impl RecordDecl {
    /// The record's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The record's fields, in order.
    pub fn fields(&self) -> impl Iterator<Item = RecordField> {
        support::children(&self.syntax)
    }
}

impl RecordField {
    /// The field's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The field's declared type.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The field's `=` default expression, if any.
    pub fn default(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl EnumDecl {
    /// The enum's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The enum's variants, in order.
    pub fn variants(&self) -> impl Iterator<Item = EnumVariant> {
        support::children(&self.syntax)
    }
}

impl EnumVariant {
    /// The variant's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }
}

impl InputDecl {
    /// The input's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The input's declared type (after `:`).
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The input's `=` default expression, if any.
    pub fn default(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl StateDecl {
    /// The state's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The state's declared type, when annotated (otherwise inferred).
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The state's `=` initializer expression.
    pub fn initializer(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl ComputedDecl {
    /// The computed's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The computed's declared type, when annotated.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The `=` expression the computed derives.
    pub fn body(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl EventDecl {
    /// The event's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }
}

impl ConstDecl {
    /// The constant's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The constant's declared type.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The `=` value expression.
    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl TypeAliasDecl {
    /// The alias's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }
}

/// Shared accessors for the three callable forms (`fn`/`action`/`task`), which
/// parse to one shape under distinct kinds.
macro_rules! callable_accessors {
    ($name:ident) => {
        impl $name {
            /// The callable's name.
            pub fn name(&self) -> Option<SyntaxToken> {
                support::name_token(&self.syntax)
            }

            /// The parameter list.
            pub fn param_list(&self) -> Option<ParamList> {
                support::child(&self.syntax)
            }

            /// The parameters, in order.
            pub fn params(&self) -> Vec<Param> {
                self.param_list()
                    .map(|l| l.params().collect())
                    .unwrap_or_default()
            }

            /// The `-> Type` return type, if declared.
            pub fn return_type(&self) -> Option<ReturnType> {
                support::child(&self.syntax)
            }

            /// The body block, if the callable has one.
            pub fn body(&self) -> Option<Block> {
                support::child(&self.syntax)
            }
        }
    };
}

callable_accessors!(FnDecl);
callable_accessors!(ActionDecl);
callable_accessors!(TaskDecl);

ast_node!(
    /// A `( Param,* )` parameter list.
    ParamList = ParamList
);
ast_node!(
    /// One `mut? IDENT : Type (= Expr)?` parameter.
    Param = Param
);
ast_node!(
    /// A `-> Type` return type.
    ReturnType = ReturnType
);
ast_node!(
    /// A `TypePathSegment (:: TypePathSegment)*` type path.
    TypePath = TypePath
);
ast_node!(
    /// A `{ ... }` statement block.
    Block = Block
);

impl ParamList {
    /// The parameters, in order.
    pub fn params(&self) -> impl Iterator<Item = Param> {
        support::children(&self.syntax)
    }
}

impl Param {
    /// The parameter's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The parameter's declared type.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }
}

impl ReturnType {
    /// The declared return type.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }
}

impl ViewDecl {
    /// The view's block of structure items.
    pub fn block(&self) -> Option<ViewBlock> {
        support::child(&self.syntax)
    }
}

// --- View --------------------------------------------------------------------

ast_node!(
    /// A `{ ViewStructureItem* }` view block.
    ViewBlock = ViewBlock
);
ast_node!(
    /// `node IDENT : ComponentType NodeBody` — a named child node.
    NamedNode = NamedNode
);
ast_node!(
    /// `ComponentType NodeBody` — an anonymous child node.
    AnonymousNode = AnonymousNode
);
ast_node!(
    /// A `{ NodeMember* }` node body.
    NodeBody = NodeBody
);
ast_node!(
    /// `PropertyPath : Expr ;` — a declarative property binding.
    PropertyBinding = PropertyBinding
);
ast_node!(
    /// An `IDENT ("." IDENT)*` property path.
    PropertyPath = PropertyPath
);
ast_node!(
    /// `bind PropertyPath <=> AssignablePath (using TypePath)? ;`.
    TwoWayBinding = TwoWayBinding
);
ast_node!(
    /// `on EventPhase? IDENT ( Pattern )? Block` — an event handler.
    EventHandler = EventHandler
);
ast_node!(
    /// `if HeadExpr ViewBlock (else ...)?` in view position.
    ViewIf = ViewIf
);
ast_node!(
    /// `for Pattern in HeadExpr key HeadExpr ViewBlock`.
    ViewFor = ViewFor
);
ast_node!(
    /// `match HeadExpr { ViewMatchArm,* }`.
    ViewMatch = ViewMatch
);
ast_node!(
    /// `Pattern (if Expr)? => ViewBlock` inside a view match.
    ViewMatchArm = ViewMatchArm
);
ast_node!(
    /// `fill IDENT ViewBlock`.
    FillClause = FillClause
);

impl ViewBlock {
    /// The structure items directly under this block.
    pub fn items(&self) -> impl Iterator<Item = ViewItem> {
        support::children(&self.syntax)
    }
}

impl NamedNode {
    /// The node's local name (before the `:`).
    pub fn name(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The node's component type (after the `:`).
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The node's body.
    pub fn body(&self) -> Option<NodeBody> {
        support::child(&self.syntax)
    }
}

impl AnonymousNode {
    /// The node's component type.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }

    /// The node's body.
    pub fn body(&self) -> Option<NodeBody> {
        support::child(&self.syntax)
    }
}

impl NodeBody {
    /// The members inside the node body, in order.
    pub fn members(&self) -> impl Iterator<Item = NodeMember> {
        support::children(&self.syntax)
    }
}

impl PropertyBinding {
    /// The bound property path (the left side, before the `:`).
    pub fn path(&self) -> Option<PropertyPath> {
        support::child(&self.syntax)
    }

    /// The bound value expression (the right side, after the `:`).
    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl PropertyPath {
    /// The path's dotted segments, in order.
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.syntax
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
    }
}

impl EventHandler {
    /// The event name this handler binds.
    pub fn event(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }

    /// The handler's block body.
    pub fn body(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

impl ViewFor {
    /// The loop body's view block.
    pub fn body(&self) -> Option<ViewBlock> {
        support::child(&self.syntax)
    }
}

impl ViewMatch {
    /// The match arms, in order.
    pub fn arms(&self) -> impl Iterator<Item = ViewMatchArm> {
        support::children(&self.syntax)
    }
}

// --- Expressions -------------------------------------------------------------

ast_node!(
    /// A literal expression (int/float/string/color/bool/…).
    LiteralExpr = LiteralExpr
);
ast_node!(
    /// A path expression (`a`, `a::b`, `self`, `Self`).
    PathExpr = PathExpr
);
ast_node!(
    /// A binary operator expression `lhs op rhs`.
    BinaryExpr = BinaryExpr
);
ast_node!(
    /// A prefix unary expression `op expr`.
    UnaryExpr = UnaryExpr
);
ast_node!(
    /// A `callee GenericCallArgs? ( args )` call.
    CallExpr = CallExpr
);
ast_node!(
    /// An `expr [ index ]` index.
    IndexExpr = IndexExpr
);
ast_node!(
    /// An `expr . IDENT` field access.
    FieldExpr = FieldExpr
);
ast_node!(
    /// An `expr ?` try.
    TryExpr = TryExpr
);
ast_node!(
    /// A `lo (.. | ..=) hi` range.
    RangeExpr = RangeExpr
);
ast_node!(
    /// An `expr as Type` cast.
    CastExpr = CastExpr
);
ast_node!(
    /// A `Path? { RecordExprField,* }` record expression.
    RecordExpr = RecordExpr
);
ast_node!(
    /// A `( Expr,* )` tuple.
    TupleExpr = TupleExpr
);
ast_node!(
    /// A `[ Expr,* ]` list.
    ListExpr = ListExpr
);
ast_node!(
    /// A `( Expr )` parenthesized expression.
    ParenExpr = ParenExpr
);
ast_node!(
    /// An `if ... else ...` expression.
    IfExpr = IfExpr
);
ast_node!(
    /// A `match ... { arm,* }` expression.
    MatchExpr = MatchExpr
);
ast_node!(
    /// A closure `move? |params| body`.
    ClosureExpr = ClosureExpr
);

impl FieldExpr {
    /// The receiver expression (`a` in `a.b`).
    pub fn receiver(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The accessed field name.
    pub fn field(&self) -> Option<SyntaxToken> {
        support::name_token(&self.syntax)
    }
}

impl CallExpr {
    /// The callee expression.
    pub fn callee(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl CastExpr {
    /// The operand being cast.
    pub fn operand(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The target type.
    pub fn ty(&self) -> Option<TypePath> {
        support::child(&self.syntax)
    }
}

impl PathExpr {
    /// The path's identifier segments, root first. A `PathExpr` holds its
    /// `IDENT ("::" IDENT)*` sequence as bare tokens, so the segments are the
    /// direct identifier children.
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.syntax
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .filter(|t| {
                matches!(
                    t.kind(),
                    SyntaxKind::Ident
                        | SyntaxKind::RawIdent
                        | SyntaxKind::SelfValueKw
                        | SyntaxKind::SelfTypeKw
                )
            })
    }
}

impl TypePath {
    /// The path's segment names, root first. Each `TypePathSegment` child owns a
    /// leading name token; this projects that name, skipping generic arguments.
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.syntax
            .children()
            .into_iter()
            .filter(|n| n.kind() == SyntaxKind::TypePathSegment)
            .filter_map(|seg| support::name_token(&seg))
    }
}

// --- Enum wrappers -----------------------------------------------------------

/// Any expression node. The catch-all typed view over the expression grammar;
/// [`Expr::cast`] accepts every expression node kind and [`Expr::syntax`] returns
/// the concrete node for further casting to a specific wrapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expr {
    syntax: SyntaxNode,
}

impl Expr {
    /// Whether `kind` is one of the expression node kinds.
    fn is_expr_kind(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::LiteralExpr
                | SyntaxKind::PathExpr
                | SyntaxKind::BinaryExpr
                | SyntaxKind::UnaryExpr
                | SyntaxKind::CallExpr
                | SyntaxKind::IndexExpr
                | SyntaxKind::FieldExpr
                | SyntaxKind::OptionalFieldExpr
                | SyntaxKind::TryExpr
                | SyntaxKind::RangeExpr
                | SyntaxKind::CastExpr
                | SyntaxKind::RecordExpr
                | SyntaxKind::TupleExpr
                | SyntaxKind::ListExpr
                | SyntaxKind::ParenExpr
                | SyntaxKind::IfExpr
                | SyntaxKind::MatchExpr
                | SyntaxKind::ClosureExpr
        )
    }
}

impl AstNode for Expr {
    fn can_cast(kind: SyntaxKind) -> bool {
        Self::is_expr_kind(kind)
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        if Self::can_cast(node.kind()) {
            Some(Expr { syntax: node })
        } else {
            None
        }
    }
    fn syntax(&self) -> &SyntaxNode {
        &self.syntax
    }
}

/// A top-level declaration in a compilation unit: a declaration, optionally
/// wrapped in `export`, or an Advanced placeholder.
///
/// Imports are *not* items — they form their own head list, reached through
/// [`CompilationUnit::imports`], so [`CompilationUnit::items`] projects only the
/// declaration tail. An `export`-prefixed declaration appears as [`Item::Export`];
/// the wrapped declaration is reached through [`ExportDecl::declaration`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Export(ExportDecl),
    Component(ComponentDecl),
    System(SystemDecl),
    Record(RecordDecl),
    Enum(EnumDecl),
    Const(ConstDecl),
    TypeAlias(TypeAliasDecl),
    Fn(FnDecl),
    Action(ActionDecl),
    Task(TaskDecl),
    Advanced(AdvancedItem),
}

impl AstNode for Item {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::ExportDecl
                | SyntaxKind::ComponentDecl
                | SyntaxKind::SystemDecl
                | SyntaxKind::RecordDecl
                | SyntaxKind::EnumDecl
                | SyntaxKind::ConstDecl
                | SyntaxKind::TypeAliasDecl
                | SyntaxKind::FnDecl
                | SyntaxKind::ActionDecl
                | SyntaxKind::TaskDecl
                | SyntaxKind::AdvancedItem
        )
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        let item = match node.kind() {
            SyntaxKind::ExportDecl => Item::Export(ExportDecl { syntax: node }),
            SyntaxKind::ComponentDecl => Item::Component(ComponentDecl { syntax: node }),
            SyntaxKind::SystemDecl => Item::System(SystemDecl { syntax: node }),
            SyntaxKind::RecordDecl => Item::Record(RecordDecl { syntax: node }),
            SyntaxKind::EnumDecl => Item::Enum(EnumDecl { syntax: node }),
            SyntaxKind::ConstDecl => Item::Const(ConstDecl { syntax: node }),
            SyntaxKind::TypeAliasDecl => Item::TypeAlias(TypeAliasDecl { syntax: node }),
            SyntaxKind::FnDecl => Item::Fn(FnDecl { syntax: node }),
            SyntaxKind::ActionDecl => Item::Action(ActionDecl { syntax: node }),
            SyntaxKind::TaskDecl => Item::Task(TaskDecl { syntax: node }),
            SyntaxKind::AdvancedItem => Item::Advanced(AdvancedItem { syntax: node }),
            _ => return None,
        };
        Some(item)
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            Item::Export(n) => n.syntax(),
            Item::Component(n) => n.syntax(),
            Item::System(n) => n.syntax(),
            Item::Record(n) => n.syntax(),
            Item::Enum(n) => n.syntax(),
            Item::Const(n) => n.syntax(),
            Item::TypeAlias(n) => n.syntax(),
            Item::Fn(n) => n.syntax(),
            Item::Action(n) => n.syntax(),
            Item::Task(n) => n.syntax(),
            Item::Advanced(n) => n.syntax(),
        }
    }
}

/// A member of a `component`/`system` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Member {
    Input(InputDecl),
    State(StateDecl),
    Computed(ComputedDecl),
    Event(EventDecl),
    Fn(FnDecl),
    Action(ActionDecl),
    Task(TaskDecl),
    View(ViewDecl),
}

impl AstNode for Member {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::InputDecl
                | SyntaxKind::StateDecl
                | SyntaxKind::ComputedDecl
                | SyntaxKind::EventDecl
                | SyntaxKind::FnDecl
                | SyntaxKind::ActionDecl
                | SyntaxKind::TaskDecl
                | SyntaxKind::ViewDecl
        )
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        let member = match node.kind() {
            SyntaxKind::InputDecl => Member::Input(InputDecl { syntax: node }),
            SyntaxKind::StateDecl => Member::State(StateDecl { syntax: node }),
            SyntaxKind::ComputedDecl => Member::Computed(ComputedDecl { syntax: node }),
            SyntaxKind::EventDecl => Member::Event(EventDecl { syntax: node }),
            SyntaxKind::FnDecl => Member::Fn(FnDecl { syntax: node }),
            SyntaxKind::ActionDecl => Member::Action(ActionDecl { syntax: node }),
            SyntaxKind::TaskDecl => Member::Task(TaskDecl { syntax: node }),
            SyntaxKind::ViewDecl => Member::View(ViewDecl { syntax: node }),
            _ => return None,
        };
        Some(member)
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            Member::Input(n) => n.syntax(),
            Member::State(n) => n.syntax(),
            Member::Computed(n) => n.syntax(),
            Member::Event(n) => n.syntax(),
            Member::Fn(n) => n.syntax(),
            Member::Action(n) => n.syntax(),
            Member::Task(n) => n.syntax(),
            Member::View(n) => n.syntax(),
        }
    }
}

/// A structure item inside a `view` block or `ui!` fragment: a node, a property,
/// a handler, a binding, or a control-flow form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewItem {
    Named(NamedNode),
    Anonymous(AnonymousNode),
    Property(PropertyBinding),
    Handler(EventHandler),
    TwoWayBinding(TwoWayBinding),
    If(ViewIf),
    For(ViewFor),
    Match(ViewMatch),
    Fill(FillClause),
}

impl AstNode for ViewItem {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::NamedNode
                | SyntaxKind::AnonymousNode
                | SyntaxKind::PropertyBinding
                | SyntaxKind::EventHandler
                | SyntaxKind::TwoWayBinding
                | SyntaxKind::ViewIf
                | SyntaxKind::ViewFor
                | SyntaxKind::ViewMatch
                | SyntaxKind::FillClause
        )
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        let item = match node.kind() {
            SyntaxKind::NamedNode => ViewItem::Named(NamedNode { syntax: node }),
            SyntaxKind::AnonymousNode => ViewItem::Anonymous(AnonymousNode { syntax: node }),
            SyntaxKind::PropertyBinding => ViewItem::Property(PropertyBinding { syntax: node }),
            SyntaxKind::EventHandler => ViewItem::Handler(EventHandler { syntax: node }),
            SyntaxKind::TwoWayBinding => ViewItem::TwoWayBinding(TwoWayBinding { syntax: node }),
            SyntaxKind::ViewIf => ViewItem::If(ViewIf { syntax: node }),
            SyntaxKind::ViewFor => ViewItem::For(ViewFor { syntax: node }),
            SyntaxKind::ViewMatch => ViewItem::Match(ViewMatch { syntax: node }),
            SyntaxKind::FillClause => ViewItem::Fill(FillClause { syntax: node }),
            _ => return None,
        };
        Some(item)
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            ViewItem::Named(n) => n.syntax(),
            ViewItem::Anonymous(n) => n.syntax(),
            ViewItem::Property(n) => n.syntax(),
            ViewItem::Handler(n) => n.syntax(),
            ViewItem::TwoWayBinding(n) => n.syntax(),
            ViewItem::If(n) => n.syntax(),
            ViewItem::For(n) => n.syntax(),
            ViewItem::Match(n) => n.syntax(),
            ViewItem::Fill(n) => n.syntax(),
        }
    }
}

/// A member of a `node` body: the same view structure items as a view block plus
/// property bindings apply, so a node body reuses [`ViewItem`].
pub type NodeMember = ViewItem;
